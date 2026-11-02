//! Thread and Turn lifecycle owned by one Run worker connection.

use crate::app_server::{JsonGrammar, JsonRpcConnection, SolicitedSink, TransportError, Wire};
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
    pub(crate) tool_name: String,
    pub(crate) source_thread_id: String,
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
    tool_name: String,
    control_correlation: Option<crate::native_consumer::ControlCorrelation>,
    successor_operation: Option<String>,
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
    FixedReadOnly,
    CallerWorkspace,
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
    Capacity(String),
    Journal(String),
    Artifact(String),
    ArtifactQuotaExceeded,
}

/// Complete ordinary Turn text, shared with pre-acceptance synthesis rendering.
pub(crate) fn validate_message_size(message: &str) -> Result<(), TurnError> {
    if message.len() > 8 * 1024 * 1024 {
        return Err(TurnError::InvalidInput(
            "message exceeds the 8 MiB request bound",
        ));
    }
    Ok(())
}

impl TurnError {
    pub(crate) fn diagnostic_kind(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "InvalidInput",
            Self::ModelMismatch { .. } => "ModelMismatch",
            Self::EffortUnsupported { .. } => "EffortUnsupported",
            Self::TurnBusy => "TurnBusy",
            Self::IdempotencyConflict { .. } => "IdempotencyConflict",
            Self::Transport { .. } => "Transport",
            Self::AppServerRejected { .. } => "AppServerRejected",
            Self::CorrelationMismatch => "CorrelationMismatch",
            Self::DuplicateTerminal => "DuplicateTerminal",
            Self::OutcomeUnknown => "OutcomeUnknown",
            Self::InteractionNotFound { .. } => "InteractionNotFound",
            Self::InteractionResponseInvalid { .. } => "InteractionResponseInvalid",
            Self::InteractionAlreadyResolved { .. } => "InteractionAlreadyResolved",
            Self::InteractionStale { .. } => "InteractionStale",
            Self::Capacity(_) => "Capacity",
            Self::Journal(_) => "Journal",
            Self::Artifact(_) => "Artifact",
            Self::ArtifactQuotaExceeded => "ArtifactQuotaExceeded",
        }
    }

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
            Self::Capacity(reason) => write!(formatter, "storage capacity exceeded: {reason}"),
            Self::Journal(reason) => write!(formatter, "durable journal failed: {reason}"),
            Self::Artifact(reason) => write!(formatter, "response artifact failed: {reason}"),
            Self::ArtifactQuotaExceeded => write!(formatter, "retained artifact quota exceeded"),
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
            Self::Capacity(reason) => MachineError::new(
                "STORAGE_CAPACITY_EXCEEDED",
                message,
                false,
                serde_json::json!({"run_id": run_id, "reason": reason, "required_action": "operator_checkpoint"}),
            ),
            Self::Journal(reason) => MachineError::new(
                "INTERNAL_ERROR",
                message,
                false,
                serde_json::json!({"invariant": reason}),
            ),
            Self::ArtifactQuotaExceeded => MachineError::new(
                "STORAGE_CAPACITY_EXCEEDED",
                message,
                false,
                serde_json::json!({"run_id":run_id,"reason":"artifact_quota_exceeded","required_action":"operator_result_retention"}),
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
    SafeActivity {
        thread_id: String,
        turn_id: String,
        data: crate::event::ClientEventData,
    },
    Intent(OperationIntent),
    SubmitIdentity(Box<crate::operation::SubmitIdentity>),
    SubmitRefusal(crate::operation::SubmitRefusal),
    CallerNativeIntent {
        operation_id: Uuid,
        method: &'static str,
    },
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
        public_resolution: Option<Box<crate::public_resolution::Receipt>>,
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

    fn workspace_ref(&self) -> Option<&str> {
        None
    }
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

impl<C: LedgerClock + 'static, F: FaultInjector + 'static> SharedLedgerJournal<C, F> {
    /// Borrow the actor's ledger with the same writer observation and event
    /// authority as an ordinary journal append. No remote or quota wait belongs
    /// inside this boundary.
    pub(crate) fn with_ledger_authority<T>(
        &self,
        append: impl FnOnce(&mut Ledger<C, F>) -> Result<T, MachineError>,
    ) -> Result<T, MachineError> {
        let poisoned = || {
            MachineError::new(
                "INTERNAL_ERROR",
                "shared ledger lock is poisoned",
                false,
                serde_json::json!({}),
            )
        };
        if let Some(authority) = &self.event_authority {
            return authority.writer.observe_locked(|writer| {
                let mut ledger = self.ledger.lock().map_err(|_| poisoned())?;
                ledger.with_event_context(
                    crate::ledger::EventAppendContext {
                        workspace_id: authority.workspace_id.clone(),
                        server_key: authority.server_key.clone(),
                        server_epoch: authority.server_epoch,
                        writer_state_revision: writer.authority_revision,
                    },
                    append,
                )
            })?;
        }
        let mut ledger = self.ledger.lock().map_err(|_| poisoned())?;
        append(&mut ledger)
    }
}

impl<C: LedgerClock + 'static, F: FaultInjector + 'static> DurableTurnJournal
    for SharedLedgerJournal<C, F>
{
    fn workspace_ref(&self) -> Option<&str> {
        self.event_authority
            .as_ref()
            .map(|authority| authority.workspace_id.as_str())
    }

    fn append_and_sync(&mut self, entry: JournalEntry) -> Result<(), TurnError> {
        let (kind, mut payload) = journal_record(entry)?;
        if let Some(authority) = &self.event_authority {
            return authority
                .writer
                .observe_locked(|writer| {
                    if kind == AuditKind::TurnStarted {
                        let mut observation = writer.clone();
                        observation.schema_version = 1;
                        observation.generation_release = None;
                        payload["acceptance_writer"] = serde_json::to_value(observation)
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

pub(crate) fn journal_ledger_error(error: crate::ledger::LedgerError) -> TurnError {
    match error {
        crate::ledger::LedgerError::Capacity(reason) => TurnError::Capacity(reason),
        other => TurnError::Journal(other.to_string()),
    }
}

fn append_journal_payload<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    ledger: &mut Ledger<C, F>,
    kind: AuditKind,
    payload: &Value,
    run_generation: u64,
) -> Result<(), TurnError> {
    if kind == AuditKind::ClientEvent {
        return ledger
            .append_activity_event(payload, run_generation)
            .map_err(journal_ledger_error);
    }
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
            .map_err(journal_ledger_error);
    }
    if kind == AuditKind::TurnTerminal {
        ledger
            .append_final_response_event(payload, run_generation)
            .map_err(journal_ledger_error)?;
    }
    let outcome = if matches!(
        kind,
        AuditKind::CleanupResult | AuditKind::LifecycleTransition
    ) {
        ledger.append_conformance_payload(kind, payload, run_generation)
    } else {
        ledger.append_required_payload(kind, payload, run_generation)
    };
    outcome.map_err(journal_ledger_error)
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
            JournalEntry::SafeActivity {
                thread_id,
                turn_id,
                data,
            } => (
                AuditKind::ClientEvent,
                Ok(serde_json::json!({"thread_id":thread_id,"turn_id":turn_id,"data":data})),
            ),
            JournalEntry::CallerNativeIntent {
                operation_id,
                method,
            } => (
                AuditKind::TurnIntent,
                serde_json::to_value(crate::conformance::CallerNativeIntent {
                    schema: "dolgorae.caller-native-intent/v1".to_owned(),
                    operation_id,
                    method: method.to_owned(),
                }),
            ),
            JournalEntry::Intent(intent) => {
                (AuditKind::IdempotencyReserved, serde_json::to_value(intent))
            }
            JournalEntry::SubmitIdentity(identity) => {
                (AuditKind::TurnIntent, serde_json::to_value(identity))
            }
            JournalEntry::SubmitRefusal(refusal) => {
                (AuditKind::TurnIntent, serde_json::to_value(refusal))
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
                public_resolution,
                resolution,
            } => (
                AuditKind::ApprovalDecided,
                Ok({
                    let mut value = serde_json::json!({"request_id":request_id.to_string(),"idempotency_key":idempotency_key,"response_sha256":response_sha256,"resolution_receipt_id":resolution_receipt_id,"resolved_at":resolved_at,"resolution":resolution});
                    if let Some(receipt) = public_resolution {
                        value["public_resolution"] = serde_json::to_value(receipt)
                            .map_err(|_| TurnError::InvalidInput("public receipt"))?;
                    }
                    value
                }),
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

/// Bounded history commitment: complete original Turn bytes and terminal count.
/// Thread metadata such as last-access timestamps is deliberately outside it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationHistory {
    pub thread_id: String,
    pub turns: u64,
    pub sha256: String,
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
    expected_thread_id: Option<String>,
    observed_thread_id: Option<String>,
    reading_thread_id: bool,
    thread_id_bytes: Vec<u8>,
    stack: Vec<Frame>,
    grammar: JsonGrammar,
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
    recovery: bool,
    root_parent_null: Option<bool>,
    parent_null_bytes: Option<usize>,
    active_name_bytes: usize,
    recovered_turn: Option<Value>,
    seen: u64,
    generation_history: bool,
    history_hash: Sha256,
    history_turns: u64,
    history_turns_array_seen: bool,
    reading_turn_status: bool,
    turn_status: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Frame {
    array: bool,
    name: Option<Vec<u8>>,
    names: BTreeSet<Vec<u8>>,
}

impl ThreadTurnSink {
    #[must_use]
    pub fn new(turn_id: &str) -> Self {
        Self {
            turn_id: turn_id.to_owned(),
            expected_thread_id: None,
            observed_thread_id: None,
            reading_thread_id: false,
            thread_id_bytes: Vec::new(),
            stack: Vec::new(),
            grammar: JsonGrammar::default(),
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
            recovery: false,
            root_parent_null: None,
            parent_null_bytes: None,
            active_name_bytes: 0,
            recovered_turn: None,
            seen: 0,
            generation_history: false,
            history_hash: Sha256::new(),
            history_turns: 0,
            history_turns_array_seen: false,
            reading_turn_status: false,
            turn_status: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn retained_parser_bytes(&self) -> usize {
        self.element.as_ref().map_or(0, Vec::capacity)
            + self.name.capacity()
            + self.element_id.capacity()
            + self.thread_id_bytes.capacity()
            + self.stack.capacity() * std::mem::size_of::<Frame>()
            + self.grammar.retained_bytes()
            + self
                .stack
                .iter()
                .map(|frame| {
                    frame.name.as_ref().map_or(0, Vec::capacity)
                        + frame.names.iter().map(Vec::capacity).sum::<usize>()
                })
                .sum::<usize>()
    }

    /// The wanted Turn's items, once the reply has been fully streamed.
    pub fn into_items(self) -> Result<Vec<Value>, TurnError> {
        self.grammar
            .finish()
            .map_err(|_| TurnError::CorrelationMismatch)?;
        if !self.stack.is_empty() || self.in_string {
            return Err(TurnError::CorrelationMismatch);
        }
        if self.expected_thread_id.is_some() && self.expected_thread_id != self.observed_thread_id {
            return Err(TurnError::CorrelationMismatch);
        }
        self.items.ok_or(TurnError::CorrelationMismatch)
    }

    pub fn new_bound(thread_id: &str, turn_id: &str) -> Self {
        let mut sink = Self::new(turn_id);
        sink.expected_thread_id = Some(thread_id.to_owned());
        sink
    }

    pub(crate) fn for_recovery(thread_id: &str, turn_id: &str) -> Self {
        let mut sink = Self::new_bound(thread_id, turn_id);
        sink.recovery = true;
        sink
    }

    pub(crate) fn into_recovery(self) -> Result<RecoveredHistory, TurnError> {
        self.grammar
            .finish()
            .map_err(|_| TurnError::CorrelationMismatch)?;
        if !self.stack.is_empty()
            || self.in_string
            || self.expected_thread_id != self.observed_thread_id
            || self.root_parent_null != Some(true)
        {
            return Err(TurnError::CorrelationMismatch);
        }
        let turn = self.recovered_turn.ok_or(TurnError::CorrelationMismatch)?;
        let status = turn
            .get("status")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "completed" | "failed" | "interrupted"))
            .ok_or(TurnError::CorrelationMismatch)?
            .to_owned();
        Ok(RecoveredHistory {
            thread_id: self
                .expected_thread_id
                .ok_or(TurnError::CorrelationMismatch)?,
            turn_id: self.turn_id,
            status,
            full: turn.get("itemsView").and_then(Value::as_str) == Some("full")
                && self.items.is_some(),
            items: self.items.unwrap_or_default(),
            usage: turn.get("usage").cloned(),
        })
    }

    pub(crate) fn for_generation(thread: &str) -> Self {
        let mut sink = Self::for_recovery(thread, "");
        sink.generation_history = true;
        sink
    }

    pub(crate) fn into_generation(self) -> Result<GenerationHistory, TurnError> {
        self.grammar
            .finish()
            .map_err(|_| TurnError::CorrelationMismatch)?;
        if !self.generation_history
            || !self.history_turns_array_seen
            || !self.stack.is_empty()
            || self.in_string
            || self.expected_thread_id != self.observed_thread_id
            || self.root_parent_null != Some(true)
        {
            return Err(TurnError::CorrelationMismatch);
        }
        Ok(GenerationHistory {
            thread_id: self
                .observed_thread_id
                .ok_or(TurnError::CorrelationMismatch)?,
            turns: self.history_turns,
            sha256: data_encoding::HEXLOWER.encode(&self.history_hash.finalize()),
        })
    }

    #[must_use]
    pub const fn streamed_bytes(&self) -> u64 {
        self.seen
    }

    fn inside_turns_array(&self) -> bool {
        (!self.recovery
            || (self.stack.len() == 3
                && self
                    .stack
                    .get(1)
                    .is_some_and(|frame| !frame.array && frame.name.as_deref() == Some(b"thread"))))
            && self
                .stack
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
        if self.generation_history {
            if self.element_id.is_empty()
                || !matches!(
                    self.turn_status.as_slice(),
                    b"completed" | b"failed" | b"interrupted"
                )
            {
                return Err(TurnError::OutcomeUnknown);
            }
            self.history_turns = self
                .history_turns
                .checked_add(1)
                .ok_or(TurnError::OutcomeUnknown)?;
            self.element = None;
            return Ok(());
        }
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
        if self.items.is_some() || self.recovered_turn.is_some() {
            return Err(TurnError::CorrelationMismatch);
        }
        let items = turn.get("items").and_then(Value::as_array).cloned();
        if items
            .as_ref()
            .is_some_and(|items| items.len() > MAX_TURN_ITEMS)
            || (!self.recovery && items.is_none())
        {
            return Err(TurnError::CorrelationMismatch);
        }
        self.items = items;
        if self.recovery {
            self.recovered_turn = Some(turn);
        }
        Ok(())
    }

    fn step(&mut self, byte: u8) -> Result<(), TurnError> {
        self.seen = self.seen.saturating_add(1);
        if self.generation_history && self.element_active {
            self.history_hash.update([byte]);
        }
        self.grammar
            .push(byte)
            .map_err(|_| TurnError::CorrelationMismatch)?;
        if let Some(element) = self.element.as_mut() {
            if element.len() >= MAX_SOLICITED_TURN_BYTES {
                return Err(TurnError::CorrelationMismatch);
            }
            element.push(byte);
        }
        if self.in_string {
            if self.escaped {
                self.escaped = false;
                if self.reading_name {
                    self.push_name_byte(byte)?;
                }
            } else if byte == b'\\' {
                if self.reading_thread_id
                    || self.reading_turn_status
                    || (self.generation_history && self.reading_element_id)
                {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.escaped = true;
                if self.reading_name {
                    self.push_name_byte(byte)?;
                }
            } else if byte == b'"' {
                self.in_string = false;
                self.last_significant = b'"';
                if self.reading_name {
                    self.reading_name = false;
                    let mut encoded = vec![b'"'];
                    encoded.extend(std::mem::take(&mut self.name));
                    encoded.push(b'"');
                    let name = serde_json::from_slice::<String>(&encoded)
                        .map_err(|_| TurnError::CorrelationMismatch)?
                        .into_bytes();
                    if self.recovery {
                        let size = self
                            .active_name_bytes
                            .checked_add(name.len())
                            .filter(|size| *size <= MAX_SOLICITED_TURN_BYTES)
                            .ok_or(TurnError::CorrelationMismatch)?;
                        if !self
                            .stack
                            .last_mut()
                            .ok_or(TurnError::CorrelationMismatch)?
                            .names
                            .insert(name.clone())
                        {
                            return Err(TurnError::CorrelationMismatch);
                        }
                        self.active_name_bytes = size;
                    }
                    self.pending_name = Some(name);
                } else if self.reading_thread_id {
                    self.reading_thread_id = false;
                    if self.observed_thread_id.is_some() {
                        return Err(TurnError::CorrelationMismatch);
                    }
                    self.observed_thread_id = Some(
                        String::from_utf8(std::mem::take(&mut self.thread_id_bytes))
                            .map_err(|_| TurnError::CorrelationMismatch)?,
                    );
                } else if self.reading_turn_status {
                    self.reading_turn_status = false;
                } else if self.reading_element_id {
                    self.reading_element_id = false;
                    self.discard_unless_wanted();
                }
            } else if self.reading_name {
                self.push_name_byte(byte)?;
            } else if self.reading_thread_id {
                if self.thread_id_bytes.len() >= 256 {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.thread_id_bytes.push(byte);
            } else if self.reading_turn_status {
                if self.turn_status.len() >= 32 {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.turn_status.push(byte);
            } else if self.reading_element_id {
                if self.element_id.len() >= 256 {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.element_id.push(byte);
            }
            return Ok(());
        }
        if let Some(position) = self.parent_null_bytes {
            if position < 4 {
                if byte != b"null"[position] {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.parent_null_bytes = Some(position + 1);
                self.last_significant = byte;
                return Ok(());
            }
            if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
                return Ok(());
            }
            if !matches!(byte, b',' | b'}') {
                return Err(TurnError::CorrelationMismatch);
            }
            self.parent_null_bytes = None;
            self.root_parent_null = Some(true);
        }
        if self.recovery
            && !byte.is_ascii_whitespace()
            && self.stack.len() == 2
            && self
                .stack
                .last()
                .is_some_and(|frame| !frame.array && frame.name.as_deref() == Some(b"thread"))
            && self.last_significant == b':'
            && self.pending_name.as_deref() == Some(b"parentThreadId")
        {
            if self.root_parent_null.is_some() {
                return Err(TurnError::CorrelationMismatch);
            }
            if byte != b'n' {
                return Err(TurnError::CorrelationMismatch);
            }
            self.root_parent_null = Some(false);
            self.parent_null_bytes = Some(1);
            self.last_significant = byte;
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
                } else if self.expected_thread_id.is_some()
                    && self.stack.len() == 2
                    && self.stack.last().is_some_and(|frame| {
                        !frame.array && frame.name.as_deref() == Some(b"thread")
                    })
                    && self.last_significant == b':'
                    && self.pending_name.as_deref() == Some(b"id")
                {
                    self.reading_thread_id = true;
                    self.thread_id_bytes.clear();
                } else if self.generation_history
                    && self.element_active
                    && self.last_significant == b':'
                    && self.stack.len() == self.element_depth + 1
                    && self.pending_name.as_deref() == Some(b"status")
                {
                    self.reading_turn_status = true;
                    self.turn_status.clear();
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
                if self.generation_history
                    && array
                    && self.stack.len() == 2
                    && self
                        .stack
                        .last()
                        .is_some_and(|f| f.name.as_deref() == Some(b"thread"))
                    && self.pending_name.as_deref() == Some(b"turns")
                {
                    self.history_turns_array_seen = true;
                }
                if byte == b'{' && !self.element_active && self.inside_turns_array() {
                    self.element = (!self.generation_history).then(|| vec![b'{']);
                    if self.generation_history {
                        self.history_hash.update(b"{");
                        self.turn_status.clear();
                    }
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
                    names: BTreeSet::new(),
                });
            }
            b'}' | b']' => {
                let frame = self.stack.pop().ok_or(TurnError::CorrelationMismatch)?;
                for name in &frame.names {
                    self.active_name_bytes -= name.len();
                }
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

    fn push_name_byte(&mut self, byte: u8) -> Result<(), TurnError> {
        if self.name.len() >= 256 {
            return Err(TurnError::CorrelationMismatch);
        }
        self.name.push(byte);
        Ok(())
    }
}

/// Fresh, solicited terminal evidence. Full response evidence is a separate bit.
#[derive(Clone)]
pub(crate) struct RecoveredHistory {
    pub thread_id: String,
    pub turn_id: String,
    pub status: String,
    pub full: bool,
    pub items: Vec<Value>,
    pub usage: Option<Value>,
}

impl RecoveredHistory {
    pub(crate) fn response_text(&self) -> Option<String> {
        let mut fallback = None;
        let mut answer = None;
        for item in &self.items {
            if item.get("type").and_then(Value::as_str) != Some("agentMessage")
                || item
                    .get("threadId")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v != self.thread_id)
                || item
                    .get("turnId")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v != self.turn_id)
                || item.get("parentThreadId").is_some_and(|v| !v.is_null())
                || item
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v != "completed")
            {
                continue;
            }
            let Some(text) = item.get("text").and_then(Value::as_str) else {
                continue;
            };
            match item.get("phase") {
                Some(Value::String(phase)) if phase == "final_answer" => {
                    answer = Some(text.to_owned())
                }
                None | Some(Value::Null) => fallback = Some(text.to_owned()),
                _ => {}
            }
        }
        answer.or(fallback)
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
    recovery_lifecycle: Option<RunLifecycle>,
    terminal_turn_ids: BTreeSet<String>,
    pending: BTreeMap<u64, Interaction>,
    resolved_interactions: BTreeMap<u64, InteractionResolution>,
    public_request_ids: BTreeMap<u64, Uuid>,
    public_resolutions: Vec<crate::public_resolution::Receipt>,
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
    refused_idempotency: BTreeMap<String, String>,
    fixed_model: String,
    default_effort: String,
    supported_efforts: BTreeSet<String>,
    cwd: PathBuf,
    developer_instructions: String,
    sandbox: String,
    approval_policy: String,
    safety_policy: SessionSafetyPolicy,
    fixed_readonly_verified: bool,
    run_generation: u64,
    server_key: String,
    server_epoch: u64,
    run_id: Uuid,
    controller_id: Uuid,
    control_mode: String,
    record_accepted_user_input: bool,
    foreign_diagnostics: Box<dyn ForeignDiagnostics>,
    primary_tool: Option<PrimaryToolConfig>,
    candidate_tools: bool,
    candidate_reader_required: bool,
    candidate_reader: Option<crate::direct_review_target::DirectTargetReader>,
    pending_terminal_history: Option<RecoveredHistory>,
    candidate_history_failure: Option<crate::machine::MachineError>,
    defer_checked_history_publication: bool,
    /// Where a successor-bound Turn publishes its file-change payloads (MD1).
    file_change_publication_dir: Option<PathBuf>,
    pending_primary_tools: BTreeMap<String, PendingPrimaryToolCall>,
    caller_runtime: Option<crate::worker::QualifiedCallerRuntime>,
    last_effective_policy: Option<crate::domain::EffectivePolicy>,
    caller_startup_phase: Option<&'static str>,
    caller_startup_failure: Option<Value>,
    successor_interactions: bool,
    controller_answers: bool,
    activity_items: BTreeSet<(String, String, bool)>,
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
    pub candidate_tools: bool,
    pub candidate_reader: Option<crate::direct_review_target::DirectTargetReader>,
    /// Where a successor-bound Turn publishes its normalized file-change payloads.
    ///
    /// Only an External Specialist member Run sets this; a legacy Turn leaves it
    /// `None` and publishes nothing (MD1).
    pub file_change_publication_dir: Option<PathBuf>,
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
            || config.cwd.to_str().is_none()
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
        if config.candidate_tools
            && (config.primary_tool.is_some()
                || !matches!(
                    config.safety_policy,
                    SessionSafetyPolicy::ReviewerReadOnly | SessionSafetyPolicy::FixedReadOnly
                )
                || config.sandbox != "read-only"
                || config.approval_policy != "never"
                || matches!(config.attach, ThreadAttach::Fork { .. }))
        {
            return Err(TurnError::InvalidInput("candidate tool session is invalid"));
        }
        if let Some(reader) = &config.candidate_reader
            && (!config.candidate_tools
                || config.primary_tool.is_some()
                || !matches!(
                    config.safety_policy,
                    SessionSafetyPolicy::ReviewerReadOnly | SessionSafetyPolicy::FixedReadOnly
                )
                || config.sandbox != "read-only"
                || config.approval_policy != "never"
                || reader.run_id() != config.run_id
                || reader
                    .workspace()
                    .map_err(|_| TurnError::CorrelationMismatch)?
                    != config.cwd
                || matches!(config.attach, ThreadAttach::Fork { .. }))
        {
            return Err(TurnError::InvalidInput(
                "direct Reviewer binding is invalid",
            ));
        }
        let experimental_api = config.primary_tool.is_some()
            || config.candidate_tools
            || matches!(
                config.safety_policy,
                SessionSafetyPolicy::CallerWorkspace | SessionSafetyPolicy::FixedReadOnly
            );
        let initialized = server.request("initialize", serde_json::json!({
            "clientInfo": {"name":"dolgorae","title":"Dolgorae","version":env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi":experimental_api,"optOutNotificationMethods":[]}
        }))?;
        if initialized.get("codexHome").and_then(Value::as_str) != Some(expected_codex_home) {
            return Err(TurnError::CorrelationMismatch);
        }
        server.notify("initialized", serde_json::json!({}))?;
        let thread_id = None;
        let state = CoordinatorState::Threadless;
        let mut coordinator = Self {
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
            recovery_lifecycle: None,
            terminal_turn_ids: BTreeSet::new(),
            pending: BTreeMap::new(),
            resolved_interactions: BTreeMap::new(),
            public_request_ids: BTreeMap::new(),
            public_resolutions: Vec::new(),
            file_change_snapshots: BTreeMap::new(),
            completed_item_count: 0,
            ignored_foreign: Vec::new(),
            undiagnosed_foreign: 0,
            streamed_history_bytes: 0,
            idempotency: BTreeMap::new(),
            unresolved_idempotency: BTreeMap::new(),
            refused_idempotency: BTreeMap::new(),
            fixed_model: config.fixed_model,
            default_effort: config.default_effort,
            supported_efforts: config.supported_efforts,
            cwd: config.cwd,
            developer_instructions: config.developer_instructions,
            sandbox: config.sandbox,
            approval_policy: config.approval_policy,
            safety_policy: config.safety_policy,
            fixed_readonly_verified: false,
            run_generation: config.run_generation,
            server_key: config.server_key,
            server_epoch: config.server_epoch,
            run_id: config.run_id,
            controller_id: config.controller_id,
            control_mode: config.control_mode,
            record_accepted_user_input: config.record_accepted_user_input,
            foreign_diagnostics: config.foreign_diagnostics,
            primary_tool: config.primary_tool,
            candidate_tools: config.candidate_tools,
            candidate_reader_required: config.candidate_tools,
            candidate_reader: config.candidate_reader,
            pending_terminal_history: None,
            candidate_history_failure: None,
            defer_checked_history_publication: false,
            file_change_publication_dir: config.file_change_publication_dir,
            pending_primary_tools: BTreeMap::new(),
            caller_runtime: None,
            last_effective_policy: None,
            caller_startup_phase: None,
            caller_startup_failure: None,
            successor_interactions: false,
            controller_answers: false,
            activity_items: BTreeSet::new(),
        };
        if coordinator.safety_policy == SessionSafetyPolicy::FixedReadOnly {
            if coordinator.sandbox != "read-only" || coordinator.approval_policy != "never" {
                return Err(TurnError::CorrelationMismatch);
            }
            coordinator.check_caller_configuration()?;
            let mut cursor = Value::Null;
            for _ in 0..16 {
                let features = coordinator.server.request(
                    "experimentalFeature/list",
                    serde_json::json!({"cursor":cursor,"limit":100}),
                )?;
                if let Some(feature) = features["data"].as_array().and_then(|items| {
                    items
                        .iter()
                        .find(|feature| feature["name"] == "multi_agent")
                }) {
                    if feature["enabled"] != false {
                        return Err(TurnError::CorrelationMismatch);
                    }
                    coordinator.fixed_readonly_verified = true;
                    break;
                }
                cursor = features["nextCursor"].clone();
                if cursor.is_null() {
                    break;
                }
            }
            if !coordinator.fixed_readonly_verified {
                return Err(TurnError::CorrelationMismatch);
            }
        }
        Ok(coordinator)
    }

    #[must_use]
    pub const fn state(&self) -> &CoordinatorState {
        &self.state
    }

    pub(crate) fn pending_terminal_history(&self) -> Option<&RecoveredHistory> {
        self.pending_terminal_history.as_ref()
    }

    pub(crate) fn set_successor_history_recovery(&mut self, enabled: bool) {
        self.defer_checked_history_publication = enabled;
    }

    pub(crate) fn candidate_history_failure(&self) -> Option<&MachineError> {
        self.candidate_history_failure.as_ref()
    }

    pub(crate) fn clear_pending_terminal_history(&mut self) {
        self.pending_terminal_history = None;
        self.candidate_history_failure = None;
    }
    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }
    pub(crate) fn primary_session_id(&self) -> Option<Uuid> {
        self.primary_tool
            .as_ref()
            .filter(|config| config.contract.name() == crate::primary_tool::PRIMARY_TOOL_NAME)
            .map(|config| config.session_id)
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

    pub(crate) fn enable_successor_interactions(&mut self, controller_answers: bool) {
        self.successor_interactions = true;
        self.controller_answers = controller_answers;
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

    pub(crate) fn qualify_caller_runtime(
        &mut self,
        mut runtime: crate::worker::QualifiedCallerRuntime,
    ) -> Result<(), TurnError> {
        if (self.safety_policy != SessionSafetyPolicy::CallerWorkspace && !self.candidate_tools)
            || self.caller_runtime.is_some()
        {
            return Err(TurnError::CorrelationMismatch);
        }
        self.check_caller_configuration()?;
        let mut diagnostic = Value::Null;
        runtime
            .prove_startup_quiescence_observed(Some(&mut diagnostic))
            .map_err(|error| {
                self.caller_startup_failure = Some(serde_json::json!({
                    "substage": "post_configuration_startup_quiescence",
                    "original_error": {"kind": "TurnBusy"},
                    "process_proof": diagnostic, "worker_error_code": error.code(),
                }));
                TurnError::TurnBusy
            })?;
        self.caller_runtime = Some(runtime);
        Ok(())
    }

    pub(crate) fn is_caller_workspace(&self) -> bool {
        matches!(
            self.safety_policy,
            SessionSafetyPolicy::CallerWorkspace | SessionSafetyPolicy::FixedReadOnly
        ) || self.candidate_tools
    }

    fn caller_startup_phase(&mut self, phase: &'static str) {
        if self.caller_startup_phase.is_some() {
            self.caller_startup_phase = Some(phase);
        }
    }

    pub(crate) fn caller_startup_failure(&self) -> Option<&Value> {
        self.caller_startup_failure.as_ref()
    }

    fn record_caller_startup_predicate(&mut self, predicate: &'static str) {
        if self.caller_startup_phase.is_some() {
            self.caller_startup_failure
                .get_or_insert_with(|| serde_json::json!({}))["refused_predicate"] =
                serde_json::json!(predicate);
        }
    }

    fn record_caller_startup_failure(&mut self, error: &TurnError) {
        let Some(phase) = self.caller_startup_phase else {
            return;
        };
        let kind = error.diagnostic_kind();
        let details = self
            .caller_startup_failure
            .get_or_insert_with(|| serde_json::json!({}));
        details["substage"] = serde_json::json!(phase);
        details["original_error"] = serde_json::json!({"kind": kind});
        match error {
            TurnError::Transport { stage, .. } => {
                details["original_error"]["transport_stage"] = serde_json::json!(stage.as_str());
            }
            TurnError::AppServerRejected { code, .. } => {
                details["original_error"]["rpc_code"] = serde_json::json!(code);
            }
            _ => {}
        }
    }

    pub(crate) fn quarantine_caller_policy(&mut self, operation_id: Uuid) {
        let _ = self.mark_unknown(operation_id);
    }

    fn check_caller_configuration(&mut self) -> Result<(), TurnError> {
        let result = self.server.request(
            "config/read",
            serde_json::json!({
                "cwd": self.cwd, "includeLayers": true,
            }),
        )?;
        let session_limit = result
            .get("layers")
            .and_then(Value::as_array)
            .is_some_and(|layers| {
                layers.iter().any(|layer| {
                    layer.pointer("/name/type").and_then(Value::as_str) == Some("sessionFlags")
                        && layer.get("disabledReason").is_none_or(Value::is_null)
                        && layer
                            .pointer("/config/project_doc_max_bytes")
                            .and_then(Value::as_u64)
                            == Some(0)
                })
            });
        if !session_limit
            || result
                .pointer("/config/project_doc_max_bytes")
                .and_then(Value::as_u64)
                != Some(0)
        {
            return Err(TurnError::CorrelationMismatch);
        }
        if self.candidate_tools
            && [
                "mcp_servers",
                "apps",
                "agents",
                "hooks",
                "notify",
                "plugins",
                "marketplaces",
                "skills",
            ]
            .iter()
            .any(|name| {
                result["config"].get(*name).is_some_and(|value| {
                    !value.is_null()
                        && !value.as_object().is_some_and(|fields| fields.is_empty())
                        && !value.as_array().is_some_and(|items| items.is_empty())
                })
            })
        {
            return Err(TurnError::CorrelationMismatch);
        }
        Ok(())
    }

    fn check_caller_process_quiescence(&mut self) -> Result<(), TurnError> {
        if self.caller_runtime.is_some() {
            let caller_context = self.caller_startup_phase;
            self.caller_startup_phase("configuration_readback");
            self.check_caller_configuration()?;
            self.caller_startup_phase("process_quiescence");
            let mut diagnostic = Value::Null;
            let result = self
                .caller_runtime
                .as_mut()
                .expect("qualified runtime")
                .prove_startup_quiescence_observed(
                    self.caller_startup_phase
                        .is_some()
                        .then_some(&mut diagnostic),
                );
            if let Err(error) = result {
                if self.caller_startup_phase.is_some() {
                    self.caller_startup_failure = Some(serde_json::json!({
                        "caller_context": caller_context,
                        "process_proof": diagnostic, "worker_error_code": error.code(),
                    }));
                }
                return Err(TurnError::TurnBusy);
            }
        }
        Ok(())
    }

    pub(crate) fn finish_caller_connection_startup(&mut self) {
        if let Some(runtime) = self.caller_runtime.as_mut() {
            runtime.finish_connection_startup();
        }
    }

    pub(crate) fn complete_caller_runtime_startup(&mut self) -> Result<(), TurnError> {
        if let Some(runtime) = &self.caller_runtime {
            let mut diagnostic = Value::Null;
            if let Err(error) = runtime.complete_startup_witness(Some(&mut diagnostic)) {
                self.caller_startup_failure = Some(serde_json::json!({
                    "process_proof": diagnostic, "worker_error_code": error.code(),
                }));
                return Err(TurnError::TurnBusy);
            }
        }
        Ok(())
    }

    pub(crate) fn caller_policy_proof(
        &mut self,
        operation_id: Uuid,
    ) -> Result<Option<crate::worker::CallerPolicyProof>, TurnError> {
        let mut diagnostic = Value::Null;
        self.caller_startup_phase("policy_proof");
        self.caller_runtime
            .as_ref()
            .map(|runtime| {
                runtime
                    .proof(
                        operation_id,
                        self.thread_id
                            .as_deref()
                            .ok_or(TurnError::CorrelationMismatch)?,
                        self.last_effective_policy
                            .clone()
                            .ok_or(TurnError::CorrelationMismatch)?,
                        self.caller_startup_phase
                            .is_some()
                            .then_some(&mut diagnostic),
                    )
                    .map_err(|error| {
                        if self.caller_startup_phase.is_some() {
                            self.caller_startup_failure = Some(serde_json::json!({
                                "process_proof": diagnostic, "worker_error_code": error.code(),
                            }));
                        }
                        TurnError::OutcomeUnknown
                    })
            })
            .transpose()
    }

    pub(crate) fn verify_caller_policy(
        &mut self,
        operation_id: Uuid,
    ) -> Result<crate::worker::CallerPolicyProof, TurnError> {
        self.caller_startup_phase = Some("policy_preconditions");
        self.caller_startup_failure = None;
        let result = (|| {
            if operation_id.get_version_num() != 7
                || (self.state != CoordinatorState::Idle
                    && !(self.state == CoordinatorState::Paused
                        && self.caller_runtime.as_ref().is_some_and(|runtime| {
                            runtime.is_pending_generation_reader(operation_id)
                        })))
                || !self.pending.is_empty()
            {
                return Err(TurnError::TurnBusy);
            }
            let thread_id = self
                .thread_id
                .clone()
                .ok_or(TurnError::CorrelationMismatch)?;
            self.check_caller_transition_quiescence(&thread_id)?;
            self.check_caller_process_quiescence()?;
            self.caller_policy_proof(operation_id)?
                .ok_or(TurnError::CorrelationMismatch)
        })();
        if let Err(error) = &result {
            self.record_caller_startup_failure(error);
        }
        self.caller_startup_phase = None;
        result
    }

    fn next_effective_policy(
        &self,
        writer_generation: Option<u64>,
        thread_generation: u64,
        write: bool,
    ) -> Result<crate::domain::EffectivePolicy, TurnError> {
        let qualified = self.caller_runtime.is_some() || self.fixed_readonly_verified;
        Ok(crate::domain::EffectivePolicy {
            access: if qualified {
                if write {
                    crate::domain::Access::Write
                } else {
                    crate::domain::Access::Read
                }
            } else {
                crate::domain::Access::Unknown
            },
            verification: if qualified {
                crate::domain::PolicyVerification::Verified
            } else {
                crate::domain::PolicyVerification::Unverified
            },
            policy_epoch: crate::domain::PolicyEpoch(
                self.policy_epoch
                    .checked_add(1)
                    .ok_or(TurnError::OutcomeUnknown)?,
            ),
            thread_generation: (thread_generation != 0).then_some(thread_generation),
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
        if write
            && matches!(
                self.safety_policy,
                SessionSafetyPolicy::ReviewerReadOnly | SessionSafetyPolicy::FixedReadOnly
            )
        {
            return Err(TurnError::InvalidInput(
                "Reviewer read-only policy cannot acquire writer access",
            ));
        }
        if self.safety_policy == SessionSafetyPolicy::CallerWorkspace {
            if self.state != CoordinatorState::Idle || !self.pending.is_empty() {
                return Err(TurnError::TurnBusy);
            }
            let thread_id = self.thread_id.clone().ok_or(TurnError::TurnBusy)?;
            self.check_caller_process_quiescence()?;
            self.check_caller_transition_quiescence(&thread_id)?;
            let sandbox = if write {
                "workspace-write"
            } else {
                "read-only"
            };
            let approval = if write { "on-request" } else { "never" };
            if self.caller_runtime.is_some() {
                self.journal
                    .append_and_sync(JournalEntry::CallerNativeIntent {
                        operation_id: transaction_id,
                        method: "thread/settings/update",
                    })?;
            }
            let update = (|| {
                self.server.request(
                    "thread/settings/update",
                    serde_json::json!({
                        "threadId": thread_id,
                        "sandboxPolicy": turn_sandbox(sandbox, &self.cwd),
                        "approvalPolicy": approval,
                    }),
                )?;
                // The native request response does not prove the applied settings.
                // Bound and validate the corresponding same-thread notification.
                for _ in 0..MAX_IGNORED_FOREIGN_REQUESTS {
                    let message = self.server.next_message()?;
                    if message.get("method").and_then(Value::as_str)
                        == Some("thread/settings/updated")
                    {
                        let params = message
                            .get("params")
                            .ok_or(TurnError::CorrelationMismatch)?;
                        if params.get("threadId").and_then(Value::as_str)
                            != Some(thread_id.as_str())
                        {
                            return Err(TurnError::CorrelationMismatch);
                        }
                        self.check_workspace_settings(
                            params
                                .get("threadSettings")
                                .ok_or(TurnError::CorrelationMismatch)?,
                            sandbox,
                            approval,
                        )?;
                        if self.caller_runtime.is_some() {
                            self.check_caller_transition_quiescence(&thread_id)?;
                            self.check_caller_process_quiescence()?;
                        }
                        return Ok(());
                    }
                    self.ingest(message)?;
                    if self.state != CoordinatorState::Idle || !self.pending.is_empty() {
                        return Err(TurnError::TurnBusy);
                    }
                }
                Err(TurnError::CorrelationMismatch)
            })();
            if let Err(error) = update {
                return Err(self.lost_after_write(transaction_id, error));
            }
        }
        let effective_policy = self.next_effective_policy(
            write.then_some(writer_generation),
            self.thread_generation,
            write,
        )?;
        self.last_effective_policy = Some(effective_policy.clone());
        let policy_epoch = effective_policy.policy_epoch.0;
        if let Err(error) = self
            .journal
            .append_and_sync(JournalEntry::WriterAccessChanged {
                write,
                writer_generation,
                transaction_id,
                effective_policy,
            })
        {
            if self.safety_policy == SessionSafetyPolicy::CallerWorkspace {
                return Err(self.lost_after_write(transaction_id, error));
            }
            return Err(error);
        }
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

    fn check_caller_transition_quiescence(&mut self, thread_id: &str) -> Result<(), TurnError> {
        self.caller_startup_phase("loaded_root_before_status");
        self.check_caller_loaded_root(thread_id)?;
        self.caller_startup_phase("thread_status_readback");
        let read = self.server.request(
            "thread/read",
            serde_json::json!({
                "threadId": thread_id, "includeTurns": false,
            }),
        )?;
        if nested_identity(&read, "thread", "id").as_deref() != Some(thread_id)
            || read.pointer("/thread/parentThreadId") != Some(&Value::Null)
        {
            self.record_caller_startup_predicate("original_root_thread");
            return Err(TurnError::CorrelationMismatch);
        }
        if read.pointer("/thread/status/type").and_then(Value::as_str) != Some("idle") {
            self.record_caller_startup_predicate("idle_thread_status");
            return Err(TurnError::TurnBusy);
        }
        self.caller_startup_phase("loaded_root_after_status");
        self.check_caller_loaded_root(thread_id)
    }

    /// Verify this actor's native thread while its mutation drain is fenced.
    /// Pooled siblings may have other loaded roots; their presence grants no
    /// shutdown authority and is not mistaken for work in this thread.
    pub(crate) fn verify_actor_passivation(&mut self) -> Result<(), TurnError> {
        if self.state != CoordinatorState::Idle
            || self.active_turn_id().is_some()
            || !self.pending_interactions().is_empty()
        {
            return Err(TurnError::TurnBusy);
        }
        let thread = self
            .thread_id()
            .ok_or(TurnError::CorrelationMismatch)?
            .to_owned();
        let read = self.server.request(
            "thread/read",
            serde_json::json!({"threadId":thread,"includeTurns":false}),
        )?;
        if nested_identity(&read, "thread", "id").as_deref() != Some(thread.as_str())
            || read.pointer("/thread/parentThreadId") != Some(&Value::Null)
        {
            return Err(TurnError::CorrelationMismatch);
        }
        if read.pointer("/thread/status/type").and_then(Value::as_str) != Some("idle") {
            return Err(TurnError::TurnBusy);
        }
        self.check_caller_process_quiescence()
    }

    fn check_caller_loaded_root(&mut self, thread_id: &str) -> Result<(), TurnError> {
        let loaded = self.server.request(
            "thread/loaded/list",
            serde_json::json!({
                "cursor": null, "limit": 2,
            }),
        )?;
        let ids = loaded
            .get("data")
            .and_then(Value::as_array)
            .ok_or(TurnError::CorrelationMismatch)?;
        if loaded.get("nextCursor") != Some(&Value::Null)
            || ids.as_slice() != [Value::String(thread_id.to_owned())]
        {
            self.record_caller_startup_predicate("exclusive_loaded_root");
            return Err(TurnError::TurnBusy);
        }
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
        self.recovery_lifecycle = Some(lifecycle);
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
        let mut startup_intent = false;
        let mut binding_complete = self.thread_id.is_some();
        for (kind, payload) in &records {
            if *kind == AuditKind::TurnIntent && payload.get("schema_version").is_some() {
                let identity: crate::operation::SubmitIdentity =
                    serde_json::from_value(payload.clone()).map_err(|_| {
                        TurnError::Journal("invalid retained Submit identity".to_owned())
                    })?;
                identity.validate()?;
                if identity.mutation_envelope.namespace.scope_ref.as_deref()
                    != Some(self.run_id.to_string().as_str())
                {
                    return Err(TurnError::Journal(
                        "retained Submit identity crossed its Run".to_owned(),
                    ));
                }
            }
            if self.is_caller_workspace()
                && *kind == AuditKind::TurnIntent
                && payload.get("schema").and_then(Value::as_str)
                    == Some("dolgorae.caller-native-intent/v1")
            {
                let intent: crate::conformance::CallerNativeIntent =
                    serde_json::from_value(payload.clone()).map_err(|_| {
                        TurnError::Journal("caller native intent shape is invalid".to_owned())
                    })?;
                intent
                    .validate()
                    .map_err(|error| TurnError::Journal(error.to_string()))?;
                startup_intent |= intent.method == "thread/start";
            }
            if self.is_caller_workspace()
                && *kind == AuditKind::TurnIntent
                && let Some(provisional) = payload.get("provisional_thread_id")
            {
                let thread = provisional
                    .as_str()
                    .filter(|value| {
                        !value.is_empty() && value.len() <= 256 && !value.contains('\0')
                    })
                    .ok_or_else(|| {
                        TurnError::Journal("durable provisional Thread is invalid".to_owned())
                    })?;
                if self
                    .thread_id
                    .as_deref()
                    .is_some_and(|accepted| accepted != thread)
                {
                    return Err(TurnError::Journal(
                        "caller recovery crossed the original Thread".to_owned(),
                    ));
                }
                self.thread_id = Some(thread.to_owned());
            }
            if *kind == AuditKind::ThreadBound
                && let Some(generation) = payload.get("thread_generation")
            {
                let generation =
                    generation
                        .as_u64()
                        .filter(|value| *value > 0)
                        .ok_or_else(|| {
                            TurnError::Journal("durable thread generation is invalid".to_owned())
                        })?;
                self.thread_generation = self.thread_generation.max(generation);
                binding_complete = true;
            }
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
        self.refused_idempotency = restored.refused;
        if self.is_caller_workspace()
            && !binding_complete
            && (startup_intent || self.thread_id.is_some())
            && self.state != CoordinatorState::OutcomeUnknown
        {
            self.mark_unknown(Uuid::now_v7())?;
        }
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
            if let Some(raw) = decision.get("public_resolution") {
                let receipt: crate::public_resolution::Receipt =
                    serde_json::from_value(raw.clone())
                        .map_err(|_| TurnError::Journal("public resolution receipt".into()))?;
                receipt.validate()?;
                if decision["resolution_receipt_id"] != receipt.operation_ref.to_string() {
                    return Err(TurnError::Journal(
                        "public resolution receipt linkage".into(),
                    ));
                }
                if self
                    .public_resolutions
                    .iter()
                    .any(|r| r.mutation_envelope.namespace == receipt.mutation_envelope.namespace)
                {
                    return Err(TurnError::Journal("duplicate public resolution key".into()));
                }
                self.public_resolutions.push(receipt);
            }
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
        if self.is_caller_workspace()
            && self.state == CoordinatorState::Idle
            && self.caller_runtime.is_some()
        {
            // An explicitly recovered, previously idle generation has already
            // attached its original Thread as a verified Reader before Ready.
            // Reconciliation acknowledges that binding without replaying history.
            let thread_id = self.thread_id.clone().ok_or(TurnError::OutcomeUnknown)?;
            if self.last_effective_policy.as_ref().is_none_or(|policy| {
                policy.access != crate::domain::Access::Read
                    || policy.verification != crate::domain::PolicyVerification::Verified
            }) || !self.pending.is_empty()
            {
                return Err(TurnError::OutcomeUnknown);
            }
            self.check_caller_transition_quiescence(&thread_id)?;
            self.check_caller_process_quiescence()?;
            return Ok(true);
        }
        if self.state != CoordinatorState::OutcomeUnknown {
            return Err(TurnError::TurnBusy);
        }
        let thread_id = self.thread_id.clone().ok_or(TurnError::OutcomeUnknown)?;
        if self.is_caller_workspace() && self.recovery_turn_id.is_none() {
            // A turnless startup has no accepted Turn to reconcile. Its exact
            // retained Thread must be idle with empty history before recovery
            // can leave uncertainty. This never restores Writer authority.
            self.check_caller_transition_quiescence(&thread_id)?;
            self.check_caller_process_quiescence()?;
            let result = self.server.request(
                "thread/read",
                serde_json::json!({"threadId":thread_id,"includeTurns":true}),
            )?;
            if nested_identity(&result, "thread", "id").as_ref() != Some(&thread_id)
                || result
                    .pointer("/thread/turns")
                    .and_then(Value::as_array)
                    .is_none_or(|turns| !turns.is_empty())
            {
                return Err(TurnError::OutcomeUnknown);
            }
            self.journal
                .append_and_sync(JournalEntry::LifecycleTransition {
                    previous: self
                        .recovery_lifecycle
                        .unwrap_or(RunLifecycle::OutcomeUnknown),
                    current: RunLifecycle::Paused,
                    terminal_seal: false,
                    interrupt_terminal_confirmed: false,
                })?;
            self.state = CoordinatorState::Paused;
            self.recovery_lifecycle = None;
            return Ok(true);
        }
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
        if let Some(reader) = &self.candidate_reader {
            if nested_identity(&result, "thread", "id").as_deref() != Some(&thread_id)
                || result
                    .pointer("/thread/parentThreadId")
                    .is_some_and(|value| !value.is_null())
            {
                return Err(TurnError::CorrelationMismatch);
            }
            let items = turns
                .iter()
                .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id.as_str()))
                .and_then(|turn| turn.get("items"))
                .and_then(Value::as_array);
            if let Some(items) = items {
                for item in items {
                    reader
                        .observe_item(&thread_id, &turn_id, item, true)
                        .map_err(|_| TurnError::CorrelationMismatch)?;
                }
            } else {
                reader
                    .record_unmediated_tool()
                    .map_err(|_| TurnError::CorrelationMismatch)?;
            }
        }
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

    pub(crate) fn generation_history(
        &mut self,
        thread: &str,
    ) -> Result<GenerationHistory, TurnError> {
        let mut sink = ThreadTurnSink::for_generation(thread);
        self.server.request_streamed(
            "thread/read",
            serde_json::json!({"threadId":thread,"includeTurns":true}),
            &mut sink,
        )?;
        sink.into_generation()
    }

    pub(crate) fn prepare_generation_retirement(&mut self) -> Result<GenerationHistory, TurnError> {
        if !matches!(
            self.state,
            CoordinatorState::Idle | CoordinatorState::Paused
        ) || self.active_turn_id.is_some()
            || !self.pending.is_empty()
            || !self.is_caller_workspace()
        {
            return Err(TurnError::TurnBusy);
        }
        let thread = self.thread_id.clone().ok_or(TurnError::OutcomeUnknown)?;
        self.check_caller_transition_quiescence(&thread)?;
        self.generation_history(&thread)
    }

    pub(crate) fn start_generation_reader(
        &mut self,
        history: &GenerationHistory,
        paused: bool,
        operation_id: Uuid,
    ) -> Result<(), TurnError> {
        // This connection has no native Thread attachment yet. History is read
        // before ensure_thread can issue ID-only resume.
        if self.generation_history(&history.thread_id)? != *history {
            return Err(TurnError::CorrelationMismatch);
        }
        let rebound = self.rebind_retained_thread_with_operation(Some(operation_id));
        self.finish_caller_connection_startup();
        rebound?;
        if self.generation_history(&history.thread_id)? != *history {
            return Err(TurnError::CorrelationMismatch);
        }
        if paused {
            self.state = CoordinatorState::Paused;
        }
        Ok(())
    }

    pub(crate) fn successor_history(
        &mut self,
        thread: &str,
        turn: &str,
    ) -> Result<RecoveredHistory, TurnError> {
        if self.thread_id.as_deref() != Some(thread) {
            return Err(TurnError::CorrelationMismatch);
        }
        let mut sink = ThreadTurnSink::for_recovery(thread, turn);
        self.server.request_streamed(
            "thread/read",
            serde_json::json!({"threadId":thread,"includeTurns":true}),
            &mut sink,
        )?;
        sink.into_recovery()
    }

    pub(crate) fn reconciliation_server(&self) -> (&str, u64) {
        (&self.server_key, self.server_epoch)
    }

    pub(crate) fn adopt_reconciled_pause(&mut self) {
        self.state = CoordinatorState::Paused;
        self.active_turn_id = None;
        self.active_turn_effort = None;
        self.active_operation_id = None;
        self.recovery_turn_id = None;
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

    /// Complete a new physical connection after handshake and durable replay.
    /// Idle retained threads must be attached to this connection before it
    /// accepts input, while initial readiness belongs in the durable ledger.
    pub(crate) fn ensure_observed_checked_history(
        &self,
        task: Uuid,
        thread: &str,
        turn: &str,
    ) -> Result<(), TurnError> {
        if let Some(reader) = self
            .candidate_reader
            .as_ref()
            .filter(|r| r.review_id() == task)
            && self
                .idempotency
                .values()
                .filter_map(|r| r.terminal.as_ref())
                .any(|t| t.thread_id == thread && t.turn_id == turn)
        {
            reader
                .begin_history(thread, turn)
                .map_err(|_| TurnError::CorrelationMismatch)?;
        }
        Ok(())
    }

    pub(crate) fn checked_history_task(&self) -> Option<Uuid> {
        self.candidate_reader
            .as_ref()
            .map(|reader| reader.review_id())
    }

    pub(crate) fn complete_connection_startup(
        &mut self,
        lifecycle: RunLifecycle,
    ) -> Result<(), TurnError> {
        if let Some(reader) = &self.candidate_reader {
            let pending = reader
                .pending_histories()
                .map_err(|_| TurnError::CorrelationMismatch)?;
            if !pending.is_empty() {
                self.rebind_retained_thread()?;
                for (thread, turn) in pending {
                    if self.thread_id.as_deref() != Some(&thread) {
                        return Err(TurnError::CorrelationMismatch);
                    }
                    if self.defer_checked_history_publication {
                        let terminal = self
                            .idempotency
                            .values()
                            .filter_map(|r| r.terminal.as_ref())
                            .find(|t| t.thread_id == thread && t.turn_id == turn);
                        let Some(terminal) = terminal else {
                            // A reconciliation attempt can leave pending
                            // history before it publishes a resolution. It
                            // must resume through explicit fresh-proof recovery.
                            continue;
                        };
                        let status = terminal.status.clone();
                        let usage = terminal.usage.clone();
                        let mut history = self.successor_history(&thread, &turn)?;
                        if !history.full || history.status != status {
                            return Err(TurnError::CorrelationMismatch);
                        }
                        history.usage = usage;
                        self.pending_terminal_history = Some(history);
                        for item in &self
                            .pending_terminal_history
                            .as_ref()
                            .expect("retained history")
                            .items
                        {
                            if let Err(error) = self
                                .candidate_reader
                                .as_ref()
                                .expect("checked reader")
                                .observe_item(&thread, &turn, item, true)
                            {
                                self.candidate_history_failure = Some(error);
                                return Err(TurnError::CorrelationMismatch);
                            }
                        }
                        // The worker publishes under the common quota lease,
                        // after rereading the retained response reference.
                        return Ok(());
                    }
                    let items = self.streamed_items(&thread, &turn)?;
                    if let Some(terminal) = self
                        .idempotency
                        .values()
                        .filter_map(|r| r.terminal.as_ref())
                        .find(|t| t.turn_id == turn && t.thread_id == thread)
                    {
                        self.pending_terminal_history = Some(RecoveredHistory {
                            thread_id: thread.clone(),
                            turn_id: turn.clone(),
                            status: terminal.status.clone(),
                            full: true,
                            items: items.clone(),
                            usage: terminal.usage.clone(),
                        });
                    }
                    let response = self.extract_final_response(&thread, &turn, &items)?;
                    self.candidate_reader
                        .as_ref()
                        .expect("checked reader")
                        .finish_history_response(&thread, &turn, &items, response)
                        .map_err(|_| TurnError::CorrelationMismatch)?;
                    self.clear_pending_terminal_history();
                }
                return Ok(());
            }
        }
        if self.is_caller_workspace() && self.state == CoordinatorState::OutcomeUnknown {
            if self.thread_id.is_none() {
                return Err(TurnError::OutcomeUnknown);
            }
            return self.rebind_retained_thread();
        }
        match lifecycle {
            RunLifecycle::Starting => {
                if self.state != CoordinatorState::Threadless {
                    return Err(TurnError::TurnBusy);
                }
                self.journal
                    .append_and_sync(JournalEntry::LifecycleTransition {
                        previous: RunLifecycle::Starting,
                        current: RunLifecycle::Idle,
                        terminal_seal: false,
                        interrupt_terminal_confirmed: false,
                    })
            }
            RunLifecycle::Idle if self.thread_id.is_some() => {
                if self.state != CoordinatorState::Idle {
                    return Err(TurnError::TurnBusy);
                }
                self.rebind_retained_thread()
            }
            RunLifecycle::OutcomeUnknown | RunLifecycle::ReconciliationRequired
                if self.is_caller_workspace() =>
            {
                if self.thread_id.is_none() {
                    // No durable native identity means no safe attach target.
                    // Never replace a possibly accepted turnless Thread.
                    return Err(TurnError::OutcomeUnknown);
                }
                self.rebind_retained_thread()
            }
            _ => Ok(()),
        }
    }

    fn rebind_retained_thread(&mut self) -> Result<(), TurnError> {
        let result = self.rebind_retained_thread_inner();
        self.finish_caller_connection_startup();
        result
    }

    fn rebind_retained_thread_inner(&mut self) -> Result<(), TurnError> {
        self.rebind_retained_thread_with_operation(None)
    }

    fn rebind_retained_thread_with_operation(
        &mut self,
        operation: Option<Uuid>,
    ) -> Result<(), TurnError> {
        if let Some(previous_thread) = self.thread_id.clone() {
            if self.is_caller_workspace() {
                self.sandbox = "read-only".to_owned();
                self.approval_policy = "never".to_owned();
                self.writer_generation = None;
            }
            let generation = self
                .thread_generation
                .checked_add(1)
                .ok_or(TurnError::OutcomeUnknown)?;
            let operation_id = operation.unwrap_or_else(Uuid::now_v7);
            self.attach = ThreadAttach::Resume {
                thread_id: previous_thread.clone(),
            };
            self.thread_id = None;
            if self.is_caller_workspace() {
                self.caller_startup_phase = Some("retained_thread_preparation");
                self.caller_startup_failure = None;
            }
            let rebound = (|| {
                let thread_id = self.ensure_thread(operation_id)?;
                self.caller_startup_phase("original_thread_binding");
                if thread_id != previous_thread {
                    self.record_caller_startup_predicate("original_thread_identity");
                    return Err(TurnError::CorrelationMismatch);
                }
                self.thread_generation = generation;
                if self.caller_runtime.is_some() {
                    self.check_caller_transition_quiescence(&thread_id)?;
                }
                self.check_caller_process_quiescence()?;
                self.caller_startup_phase("effective_policy");
                let effective_policy = self.next_effective_policy(
                    self.writer_generation,
                    generation,
                    self.sandbox == "workspace-write",
                )?;
                self.last_effective_policy = Some(effective_policy.clone());
                let policy_epoch = effective_policy.policy_epoch.0;
                self.caller_startup_phase("thread_bound_journal");
                self.journal.append_and_sync(JournalEntry::ThreadBound {
                    operation_id,
                    thread_id,
                    thread_generation: generation,
                    effective_policy,
                })?;
                self.policy_epoch = policy_epoch;
                Ok(())
            })();
            if let Err(error) = rebound {
                self.record_caller_startup_failure(&error);
                self.caller_startup_phase = None;
                return Err(self.lost_after_write(operation_id, error));
            }
            self.caller_startup_phase = None;
        }
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), TurnError> {
        if self.state != CoordinatorState::Paused {
            return Err(TurnError::TurnBusy);
        }
        self.rebind_retained_thread()?;
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
        if let Some(recorded) = self.refused_idempotency.get(&request.idempotency_key)
            && recorded != &digest
        {
            return Err(TurnError::IdempotencyConflict {
                recorded: recorded.clone(),
                observed: digest,
            });
        }
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

    pub(crate) fn uses_candidate_tools(&self) -> bool {
        self.candidate_tools
    }

    pub(crate) fn bind_candidate_reader(
        &mut self,
        reader: crate::direct_review_target::DirectTargetReader,
    ) -> Result<(), TurnError> {
        if !self.candidate_tools
            || self.state != CoordinatorState::Idle
            || self.active_turn_id.is_some()
            || !self.pending.is_empty()
            || reader.run_id() != self.run_id
            || reader
                .workspace()
                .map_err(|_| TurnError::CorrelationMismatch)?
                != self.cwd
            || self.sandbox != "read-only"
            || self.approval_policy != "never"
        {
            return Err(TurnError::CorrelationMismatch);
        }
        self.candidate_reader = Some(reader);
        self.candidate_reader_required = true;
        Ok(())
    }

    /// Only authenticated successor dispatch may select the generic task contract.
    pub(crate) fn bind_generic_successor_task(&mut self) -> Result<(), TurnError> {
        if !self.candidate_tools
            || self.state != CoordinatorState::Idle
            || self.active_turn_id.is_some()
            || !self.pending.is_empty()
        {
            return Err(TurnError::CorrelationMismatch);
        }
        self.candidate_reader = None;
        self.candidate_reader_required = false;
        Ok(())
    }

    pub fn start_turn(&mut self, request: TurnRequest) -> Result<AcceptedTurn, TurnError> {
        if self.state == CoordinatorState::OutcomeUnknown {
            return Err(TurnError::OutcomeUnknown);
        }
        if let Some(accepted) = self.replay_accepted(&request)? {
            return Ok(accepted);
        }
        if (self.candidate_reader_required && self.candidate_reader.is_none())
            || (self.candidate_tools && request.write)
        {
            return Err(TurnError::InvalidInput(
                "Reviewer requires a bound read-only target",
            ));
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
        let successor_identity = match (&self.acceptance_controller, self.journal.workspace_ref()) {
            (Some(controller), Some(workspace)) if request.idempotency_key.len() <= 256 => {
                Some(crate::operation::SubmitIdentity::prepare(
                    operation_id,
                    crate::operation::Namespace::submit(
                        controller,
                        workspace,
                        self.run_id,
                        &request.idempotency_key,
                    )?,
                    &request,
                    &effort,
                )?)
            }
            _ => None,
        };
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
        self.active_operation_id = Some(operation_id);
        self.unresolved_idempotency
            .insert(request.idempotency_key.clone(), digest.clone());
        if let Some(identity) = successor_identity
            && let Err(error) = self
                .journal
                .append_and_sync(JournalEntry::SubmitIdentity(Box::new(identity)))
        {
            self.state = CoordinatorState::OutcomeUnknown;
            return Err(error);
        }
        let thread_id = match self.ensure_thread(operation_id) {
            Ok(thread) => thread,
            Err(error) => {
                if self.definitive_submit_refusal(&error) {
                    self.resolve_submit_refusal(operation_id, &request.idempotency_key, &digest)?;
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
                if self.definitive_submit_refusal(&error) {
                    self.resolve_submit_refusal(operation_id, &request.idempotency_key, &digest)?;
                }
                return Err(self.lost_after_write(operation_id, error));
            },
        };
        let Some(turn_id) = nested_identity(&result, "turn", "id")
            .or_else(|| identity(&result, "turnId"))
            .filter(|turn| crate::operation::valid_opaque_ref(turn))
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
            let effective_policy = self.next_effective_policy(
                self.writer_generation,
                self.thread_generation,
                self.sandbox == "workspace-write",
            )?;
            self.last_effective_policy = Some(effective_policy.clone());
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
            effective_policy: self.last_effective_policy.clone().unwrap_or(
                crate::domain::EffectivePolicy {
                    access: crate::domain::Access::Unknown,
                    verification: crate::domain::PolicyVerification::Unverified,
                    policy_epoch: crate::domain::PolicyEpoch(self.policy_epoch),
                    thread_generation: Some(self.thread_generation),
                    server_epoch: Some(self.server_epoch),
                    writer_generation: self.writer_generation,
                },
            ),
            controller: self.acceptance_controller.clone(),
        })?;
        self.active_turn_id = Some(turn_id.clone());
        self.active_turn_effort = Some(effort.clone());
        self.state = CoordinatorState::Running;
        let accepted = AcceptedTurn {
            thread_id,
            turn_id,
            effort,
            replayed: false,
        };
        self.unresolved_idempotency.remove(&request.idempotency_key);
        self.refused_idempotency.remove(&request.idempotency_key);
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

    fn definitive_submit_refusal(&self, error: &TurnError) -> bool {
        matches!(error, TurnError::AppServerRejected { .. })
            && !(self.is_caller_workspace()
                && self.thread_id.is_some()
                && self.thread_generation == 0)
    }

    fn resolve_submit_refusal(
        &mut self,
        operation: Uuid,
        key: &str,
        digest: &str,
    ) -> Result<(), TurnError> {
        if let Err(error) = self.journal.append_and_sync(JournalEntry::SubmitRefusal(
            crate::operation::SubmitRefusal::new(operation, key.to_owned(), digest.to_owned()),
        )) {
            self.state = CoordinatorState::OutcomeUnknown;
            return Err(error);
        }
        self.unresolved_idempotency.remove(key);
        self.refused_idempotency
            .insert(key.to_owned(), digest.to_owned());
        self.active_operation_id = None;
        Ok(())
    }

    fn capture_accepted_input(
        &mut self,
        request: &TurnRequest,
    ) -> Result<AcceptedUserInput, TurnError> {
        const MAX_INLINE_INPUT_BYTES: usize = 1024 * 1024;
        validate_message_size(&request.message)?;
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
        if matches!(
            self.safety_policy,
            SessionSafetyPolicy::CallerWorkspace | SessionSafetyPolicy::FixedReadOnly
        ) {
            self.check_caller_process_quiescence()?;
            params["config"] = serde_json::json!({
                "project_doc_max_bytes": 0,
                "sandbox_workspace_write": {
                    "writable_roots": [self.cwd], "network_access": false,
                    "exclude_slash_tmp": false, "exclude_tmpdir_env_var": false,
                },
            });
        }
        if method == "thread/start" && self.caller_runtime.is_some() {
            params["dynamicTools"] = Value::Array(
                self.primary_tool
                    .as_ref()
                    .map_or_else(Vec::new, |tool| tool.contract.tool_specs()),
            );
        } else if method == "thread/start"
            && let Some(primary_tool) = &self.primary_tool
        {
            params["dynamicTools"] = Value::Array(primary_tool.contract.tool_specs());
        }
        if self.candidate_tools {
            if method == "thread/start" {
                let mut tools = self
                    .primary_tool
                    .as_ref()
                    .map_or_else(Vec::new, |tool| tool.contract.tool_specs());
                tools.push(crate::direct_review::candidate_tool_spec());
                params["dynamicTools"] = Value::Array(tools);
            }
            params["config"] = serde_json::json!({"project_doc_max_bytes":0});
        }
        if method == "thread/start" && self.caller_runtime.is_some() {
            self.caller_startup_phase("thread_dispatch_intent_journal");
            self.journal
                .append_and_sync(JournalEntry::CallerNativeIntent {
                    operation_id,
                    method: "thread/start",
                })?;
        }
        self.caller_startup_phase(method);
        let registered_catalog = params
            .get("dynamicTools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let result = self.server.request(method, params.take())?;
        let thread_id = nested_identity(&result, "thread", "id")
            .or_else(|| identity(&result, "threadId"))
            .ok_or(TurnError::CorrelationMismatch)?;
        self.caller_startup_phase("provisional_thread_journal");
        self.journal
            .append_and_sync(JournalEntry::ProvisionalThread {
                operation_id,
                thread_id: thread_id.clone(),
            })?;
        self.thread_id = Some(thread_id.clone());
        if self.is_caller_workspace() {
            self.caller_startup_phase("thread_settings");
            let sandbox = self.sandbox.clone();
            let approval_policy = self.approval_policy.clone();
            self.check_workspace_settings(
                &serde_json::json!({
                    "cwd": result.get("cwd"), "approvalPolicy": result.get("approvalPolicy"),
                    "sandboxPolicy": result.get("sandbox"),
                }),
                &sandbox,
                &approval_policy,
            )?;
            if self.candidate_tools
                && result.pointer("/thread/parentThreadId") != Some(&Value::Null)
            {
                return Err(TurnError::CorrelationMismatch);
            }
            if result.get("model").and_then(Value::as_str) != Some(self.fixed_model.as_str()) {
                self.record_caller_startup_predicate("thread_model");
                return Err(TurnError::CorrelationMismatch);
            }
            if matches!(&self.attach, ThreadAttach::Resume { thread_id: expected } if expected != &thread_id)
            {
                self.record_caller_startup_predicate("resumed_thread_identity");
                return Err(TurnError::CorrelationMismatch);
            }
            if matches!(self.attach, ThreadAttach::Start)
                && result
                    .pointer("/thread/turns")
                    .and_then(Value::as_array)
                    .is_none_or(|turns| !turns.is_empty())
            {
                self.record_caller_startup_predicate("empty_initial_thread_history");
                return Err(TurnError::CorrelationMismatch);
            }
            if matches!(self.attach, ThreadAttach::Start) {
                self.caller_startup_phase("thread/name/set");
                let persisted = self.server.request(
                    "thread/name/set",
                    serde_json::json!({
                        "threadId": thread_id, "name": format!("Dolgorae Run {}", self.run_id),
                    }),
                )?;
                if persisted
                    .as_object()
                    .is_none_or(|fields| !fields.is_empty())
                {
                    return Err(TurnError::CorrelationMismatch);
                }
                self.caller_startup_phase("thread_persistence_readback");
                let readback = self.server.request(
                    "thread/read",
                    serde_json::json!({
                        "threadId": thread_id, "includeTurns": true,
                    }),
                )?;
                if nested_identity(&readback, "thread", "id").as_ref() != Some(&thread_id)
                    || readback
                        .pointer("/thread/turns")
                        .and_then(Value::as_array)
                        .is_none_or(|turns| !turns.is_empty())
                {
                    return Err(TurnError::CorrelationMismatch);
                }
            }
        }
        if method == "thread/start"
            && let Some(runtime) = &self.caller_runtime
        {
            runtime
                .retain_registration(
                    &thread_id,
                    &self.fixed_model,
                    &self.default_effort,
                    &registered_catalog,
                )
                .map_err(|_| TurnError::CorrelationMismatch)?;
        }
        Ok(thread_id)
    }

    fn check_workspace_settings(
        &mut self,
        settings: &Value,
        sandbox: &str,
        approval: &str,
    ) -> Result<(), TurnError> {
        let policy = settings
            .get("sandboxPolicy")
            .ok_or(TurnError::CorrelationMismatch)?;
        let write = sandbox == "workspace-write";
        let roots_match = policy.get("writableRoots").is_some_and(|roots| {
            roots == &serde_json::json!([]) || roots == &serde_json::json!([self.cwd])
        });
        let predicates = [
            (
                "thread_settings_cwd",
                settings.get("cwd").and_then(Value::as_str) != self.cwd.to_str(),
            ),
            (
                "thread_settings_approval_policy",
                settings.get("approvalPolicy").and_then(Value::as_str) != Some(approval),
            ),
            (
                "thread_settings_sandbox_type",
                policy.get("type").and_then(Value::as_str)
                    != Some(if write { "workspaceWrite" } else { "readOnly" }),
            ),
            (
                "thread_settings_network_access",
                policy
                    .get("networkAccess")
                    .is_some_and(|value| value != &Value::Bool(false)),
            ),
            ("thread_settings_writable_roots", write && !roots_match),
            (
                "thread_settings_exclude_slash_tmp",
                write
                    && policy
                        .get("excludeSlashTmp")
                        .is_some_and(|value| value != &Value::Bool(false)),
            ),
            (
                "thread_settings_exclude_tmpdir",
                write
                    && policy
                        .get("excludeTmpdirEnvVar")
                        .is_some_and(|value| value != &Value::Bool(false)),
            ),
        ];
        if let Some((predicate, _)) = predicates.into_iter().find(|(_, refused)| *refused) {
            self.record_caller_startup_predicate(predicate);
            return Err(TurnError::CorrelationMismatch);
        }
        Ok(())
    }

    /// Create and durably bind a thread before the first task Turn. Brokered
    /// Specialist provisioning uses the spawn operation as the binding
    /// identity so a ready member never exposes a threadless publication gap.
    pub(crate) fn initialize_thread(&mut self, operation_id: Uuid) -> Result<(), TurnError> {
        let result = self.initialize_thread_inner(operation_id);
        self.finish_caller_connection_startup();
        result
    }

    fn initialize_thread_inner(&mut self, operation_id: Uuid) -> Result<(), TurnError> {
        if self.state == CoordinatorState::OutcomeUnknown {
            return Err(TurnError::OutcomeUnknown);
        }
        if self.thread_id.is_some() {
            return Ok(());
        }
        if self.state != CoordinatorState::Threadless {
            return Err(TurnError::TurnBusy);
        }
        if self.is_caller_workspace() {
            self.caller_startup_phase = Some("thread_preparation");
            self.caller_startup_failure = None;
        }
        let thread_id = match self.ensure_thread(operation_id) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                self.record_caller_startup_failure(&error);
                self.caller_startup_phase = None;
                return Err(self.lost_after_write(operation_id, error));
            }
        };
        let binding = (|| {
            self.caller_startup_phase("thread_generation");
            let thread_generation = self
                .thread_generation
                .checked_add(1)
                .ok_or(TurnError::OutcomeUnknown)?;
            if self.caller_runtime.is_some() {
                self.check_caller_transition_quiescence(&thread_id)?;
                self.check_caller_process_quiescence()?;
            }
            self.caller_startup_phase("effective_policy");
            let effective_policy = self.next_effective_policy(
                self.writer_generation,
                thread_generation,
                self.sandbox == "workspace-write",
            )?;
            self.last_effective_policy = Some(effective_policy.clone());
            let policy_epoch = effective_policy.policy_epoch.0;
            self.caller_startup_phase("thread_bound_journal");
            self.journal.append_and_sync(JournalEntry::ThreadBound {
                operation_id,
                thread_id,
                thread_generation,
                effective_policy,
            })?;
            Ok((thread_generation, policy_epoch))
        })();
        let (thread_generation, policy_epoch) = match binding {
            Ok(binding) => binding,
            Err(error) => {
                self.record_caller_startup_failure(&error);
                self.caller_startup_phase = None;
                return Err(self.lost_after_write(operation_id, error));
            }
        };
        self.caller_startup_phase = None;
        self.thread_generation = thread_generation;
        self.policy_epoch = policy_epoch;
        self.state = CoordinatorState::Idle;
        Ok(())
    }

    fn mark_unknown(&mut self, operation_id: Uuid) -> Result<(), TurnError> {
        if self.state == CoordinatorState::OutcomeUnknown {
            return Ok(());
        }
        if self.active_turn_id.is_some() {
            self.recovery_turn_id = self.active_turn_id.clone();
        }
        self.active_turn_id = None;
        self.active_turn_effort = None;
        self.active_operation_id = None;
        self.state = CoordinatorState::OutcomeUnknown;
        self.recovery_lifecycle = Some(RunLifecycle::OutcomeUnknown);
        self.journal
            .append_and_sync(JournalEntry::OutcomeUnknown { operation_id })
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
        // A correlated refusal before provisional caller startup is a known
        // no-op. Once that Thread exists, refusal of its persistence barrier
        // must still quarantine the partially completed startup.
        if matches!(error, TurnError::AppServerRejected { .. })
            && !(self.is_caller_workspace()
                && self.thread_id.is_some()
                && self.thread_generation == 0)
        {
            return error;
        }
        if let Err(journal) = self.mark_unknown(operation_id) {
            return journal;
        }
        match error {
            TurnError::Transport { .. }
            | TurnError::AppServerRejected { .. }
            | TurnError::TurnBusy => TurnError::OutcomeUnknown,
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
            "thread/settings/updated"
                if self.safety_policy == SessionSafetyPolicy::CallerWorkspace =>
            {
                // A settings notification outside the serialized update is stale
                // or duplicated; never let it qualify a later transition.
                Err(self.lost_after_write(Uuid::now_v7(), TurnError::CorrelationMismatch))
            }
            "item/started" => {
                self.observe_candidate_item(&params, false)?;
                self.capture_safe_activity(&params, false)?;
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
            "error" => {
                if params.get("threadId").and_then(Value::as_str) == self.thread_id.as_deref()
                    && event_turn_id(&params) == self.active_turn_id.as_deref()
                {
                    self.runtime_error_activity()?;
                }
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
        // A successor-bound Turn publishes the normalized payload durably before
        // any terminal resolution, so settlement can read exactly these bytes.
        if let Some(directory) = self.file_change_publication_dir.as_deref() {
            publish_file_change_payload(directory, &snapshot_sha256, &bytes)?;
        }
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
                        public_resolution: None,
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
        let current_turn = self.active_turn_id.clone();
        let turn_id = event_turn_id(params).or_else(|| {
            (self.successor_interactions && method == "mcpServer/elicitation/request")
                .then_some(current_turn.as_deref())
                .flatten()
        });
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
            if self.candidate_tools
                && !self.primary_tool.as_ref().is_some_and(|tool| {
                    params.get("tool").and_then(Value::as_str) == Some(tool.contract.name())
                })
            {
                return self.open_candidate_tool(request_id, params);
            }
            return self.open_primary_tool(request_id, params);
        }
        if let Some(reader) = &self.candidate_reader {
            reader
                .record_unmediated_tool()
                .map_err(|_| TurnError::CorrelationMismatch)?;
        }
        let supported = matches!(
            method,
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/tool/requestUserInput"
        );
        let successor =
            self.successor_interactions && crate::successor_interaction::supported(method);
        let supported = supported || successor;
        let recognized_unsupported = crate::successor_interaction::supported(method) && !successor;
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
        if !successor
            && matches!(
                self.safety_policy,
                SessionSafetyPolicy::ReviewerReadOnly | SessionSafetyPolicy::FixedReadOnly
            )
        {
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
        let policy = if successor {
            let representable = if method == "item/permissions/requestApproval" {
                crate::successor_interaction::permission(
                    params,
                    &self.cwd,
                    self.writer_generation.is_some(),
                )
                .1
            } else {
                crate::successor_interaction::mcp(params).1
            };
            if !self.controller_answers {
                Some("SPECIALIST_INTERACTION_UNSUPPORTED")
            } else if matches!(
                self.safety_policy,
                SessionSafetyPolicy::FixedReadOnly | SessionSafetyPolicy::ReviewerReadOnly
            ) {
                Some("READ_ONLY_POLICY")
            } else if !representable {
                Some("RUNTIME_VARIANT_UNSUPPORTED")
            } else {
                None
            }
        } else {
            None
        };
        self.public_request_ids
            .insert(request_id, public_request_id);
        self.pending.insert(request_id, interaction);
        self.state = CoordinatorState::WaitingInteraction;
        if let Some(cause) = policy {
            let body = if method == "item/permissions/requestApproval" {
                serde_json::json!({"decision":"decline","granted_permissions":[]})
            } else {
                serde_json::json!({"decision":"decline","answers":{}})
            };
            self.respond_with_policy(
                request_id,
                format!("policy:{public_request_id}"),
                body,
                Some(cause),
            )?;
        }
        Ok(())
    }

    fn open_candidate_tool(&mut self, request_id: u64, params: &Value) -> Result<(), TurnError> {
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        if self.thread_id.as_deref() != Some(thread_id)
            || self.active_turn_id.as_deref() != Some(turn_id)
        {
            return Err(TurnError::CorrelationMismatch);
        }
        let Some(reader) = self.candidate_reader.as_ref() else {
            let body = serde_json::json!({"error":MachineError::invalid_argument(
                "candidate_read", "current task has no checked review target"
            )});
            let text = serde_json::to_string(&body).map_err(|_| TurnError::CorrelationMismatch)?;
            return self.server.respond_result(
                request_id,
                serde_json::json!({
                    "success":false,"contentItems":[{"type":"inputText","text":text}]
                }),
            );
        };
        let result = if params.get("tool").and_then(Value::as_str)
            != Some(crate::direct_review::CANDIDATE_TOOL_NAME)
        {
            reader
                .record_unmediated_tool()
                .map_err(|_| TurnError::CorrelationMismatch)?;
            Err(crate::direct_review_target::provenance_invalid(
                reader.review_id(),
            ))
        } else {
            let call_id = params
                .get("callId")
                .and_then(Value::as_str)
                .ok_or(TurnError::CorrelationMismatch)?;
            let arguments = params
                .get("arguments")
                .cloned()
                .ok_or(TurnError::CorrelationMismatch)?;
            if serde_json::to_vec(&arguments)
                .map_err(|_| TurnError::CorrelationMismatch)?
                .len()
                > 16384
            {
                return Err(TurnError::CorrelationMismatch);
            }
            match serde_json::from_value::<crate::direct_review_target::CandidateReadRequest>(
                arguments,
            ) {
                Ok(request) => reader.read(thread_id, turn_id, call_id, &request),
                Err(_) => Err(MachineError::invalid_argument(
                    "candidate_read",
                    "read request does not match the checked tool contract",
                )),
            }
        };
        let (success, body) = match result {
            Ok(value) => (true, value),
            Err(error) => (false, serde_json::json!({"error":error})),
        };
        let text = serde_json::to_string(&body).map_err(|_| TurnError::CorrelationMismatch)?;
        self.server.respond_result(request_id,serde_json::json!({"success":success,"contentItems":[{"type":"inputText","text":text}]}))
    }

    fn observe_candidate_item(&self, params: &Value, completed: bool) -> Result<(), TurnError> {
        if let Some(reader) = &self.candidate_reader {
            if params.get("threadId").and_then(Value::as_str) != self.thread_id.as_deref() {
                return Ok(());
            }
            let turn_id = event_turn_id(params).ok_or(TurnError::CorrelationMismatch)?;
            if self.active_turn_id.as_deref() != Some(turn_id) {
                return Err(TurnError::CorrelationMismatch);
            }
            reader
                .observe_item(
                    self.thread_id
                        .as_deref()
                        .ok_or(TurnError::CorrelationMismatch)?,
                    turn_id,
                    params.get("item").ok_or(TurnError::CorrelationMismatch)?,
                    completed,
                )
                .map_err(|_| TurnError::CorrelationMismatch)?;
        }
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
        let tool = params
            .get("tool")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        let successor = contract.allows_specialists() && crate::native_consumer::managed_tool(tool);
        if tool != contract.name() && !successor {
            self.server
                .respond_error(request_id, -32601, "tool not registered")?;
            return Ok(());
        }
        let payload = params
            .get("arguments")
            .cloned()
            .ok_or(TurnError::CorrelationMismatch)?;
        let native_tool = successor.then_some(tool);
        let successor_operation = successor.then(|| {
            payload["operation"]
                .as_str()
                .filter(|operation| crate::specialist_facade::MANAGED.contains(operation))
                .unwrap_or("lookup_operation")
                .to_owned()
        });
        let raw = serde_json::to_vec(&payload).map_err(|_| TurnError::CorrelationMismatch)?;
        let control_correlation = native_tool
            .filter(|tool| crate::native_consumer::is_control(tool))
            .and_then(|_| crate::native_consumer::Control::parse(&raw).ok())
            .map(|control| control.correlation());
        if raw.len()
            > if successor {
                crate::specialist_facade::PUBLIC_LIMIT
            } else {
                MAX_INTERACTION_PAYLOAD_BYTES
            }
        {
            return Err(TurnError::CorrelationMismatch);
        }
        if let Err(error) = if successor {
            crate::host_invocation::normalize_tool_request(tool, &payload).map(|_| ())
        } else {
            contract.reject_model_identity(&payload)
        } {
            return self.respond_primary_tool(
                request_id,
                Err(error),
                successor_operation.as_deref(),
                native_tool,
                control_correlation.as_ref(),
            );
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
            if pending.payload_sha256 != format!("{tool}:{payload_sha256}") {
                return self.respond_primary_tool(
                    request_id,
                    Err(MachineError::new(
                        "IDEMPOTENCY_CONFLICT",
                        "tool call identity was reused with different input",
                        false,
                        serde_json::json!({"source_tool_call_id":call_id}),
                    )),
                    successor_operation.as_deref(),
                    native_tool,
                    control_correlation.as_ref(),
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
                successor_operation.as_deref(),
                native_tool,
                control_correlation.as_ref(),
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
            tool_name: tool.into(),
            source_thread_id: thread_id.into(),
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
                tool_name: tool.into(),
                control_correlation: control_correlation.clone(),
                successor_operation: successor_operation.clone(),
                payload_sha256: format!("{tool}:{payload_sha256}"),
                request_ids: vec![request_id],
            },
        );
        if let Err(error) = dispatcher.dispatch(call) {
            self.pending_primary_tools.remove(call_id);
            return self.respond_primary_tool(
                request_id,
                Err(error),
                successor_operation.as_deref(),
                native_tool,
                control_correlation.as_ref(),
            );
        }
        Ok(())
    }

    pub fn complete_primary_tool(
        &mut self,
        completion: PrimaryToolCompletion,
    ) -> Result<(), TurnError> {
        self.complete_primary_tool_with(completion, |_| Ok(()))
    }

    pub(crate) fn complete_primary_tool_with(
        &mut self,
        completion: PrimaryToolCompletion,
        publish: impl FnOnce(&mut J) -> Result<(), TurnError>,
    ) -> Result<(), TurnError> {
        if completion.run_generation != self.run_generation {
            return Ok(());
        }
        let Some(pending) = self
            .pending_primary_tools
            .get(&completion.source_tool_call_id)
            .cloned()
        else {
            return Ok(());
        };
        if !pending.request_ids.contains(&completion.request_id) {
            return Err(TurnError::CorrelationMismatch);
        }
        publish(&mut self.journal)?;
        self.pending_primary_tools
            .remove(&completion.source_tool_call_id);
        for request_id in pending.request_ids {
            self.respond_primary_tool(
                request_id,
                completion.result.clone(),
                pending.successor_operation.as_deref(),
                Some(&pending.tool_name),
                pending.control_correlation.as_ref(),
            )?;
        }
        Ok(())
    }

    fn respond_primary_tool(
        &mut self,
        request_id: u64,
        result: Result<Value, MachineError>,
        successor_operation: Option<&str>,
        native_tool: Option<&str>,
        control_correlation: Option<&crate::native_consumer::ControlCorrelation>,
    ) -> Result<(), TurnError> {
        if let Some(tool) = native_tool.filter(|tool| crate::native_consumer::is_native(tool)) {
            let body = match result {
                Ok(value) => value,
                Err(error) if crate::native_consumer::is_control(tool) => {
                    crate::specialist_facade::control_failure_for(control_correlation, &error)
                }
                Err(error) => crate::consumer_bridge::semantic_presentation(
                    &crate::specialist_facade::failure(
                        successor_operation.unwrap_or("lookup_operation"),
                        &error,
                        false,
                    ),
                ),
            };
            let (success, content) = match crate::consumer_bridge::native_content(&body, true) {
                Ok(content) => (!crate::consumer_bridge::native_failed(&body), content),
                Err(_) => (
                    false,
                    serde_json::json!([{"type":"inputText","text":crate::consumer_bridge::presentation_failure(&body).to_string()}]),
                ),
            };
            return self.server.respond_result(
                request_id,
                serde_json::json!({"success":success,"contentItems":content}),
            );
        }
        let (success, body) = match (successor_operation, result) {
            (Some(_), Ok(value)) => (
                value["schema"] != "dolgorae.external-specialist-failure/v5",
                value,
            ),
            (Some(operation), Err(error)) => (
                false,
                crate::specialist_facade::failure(operation, &error, false),
            ),
            (None, Ok(value)) => (true, value),
            (None, Err(error)) => {
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
                "item/permissions/requestApproval" => (
                    "permission_request",
                    serde_json::to_value(
                        crate::successor_interaction::permission(
                            &interaction.payload,
                            &self.cwd,
                            self.writer_generation.is_some(),
                        )
                        .0,
                    )
                    .map_err(|_| TurnError::CorrelationMismatch)?,
                    serde_json::json!(["accept_once", "decline", "cancel"]),
                    "dolgorae.interaction.permission/v2",
                ),
                "mcpServer/elicitation/request" => {
                    let (payload, representable) =
                        crate::successor_interaction::mcp(&interaction.payload);
                    (
                        "mcp_elicitation",
                        serde_json::to_value(payload)
                            .map_err(|_| TurnError::CorrelationMismatch)?,
                        if representable {
                            serde_json::json!(["accept_once", "decline", "cancel"])
                        } else {
                            serde_json::json!(["decline", "cancel"])
                        },
                        "dolgorae.interaction.mcp-elicitation/v2",
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
        let mut normalized = serde_json::json!({
            "schema_version": if crate::successor_interaction::supported(&interaction.method) {2} else {1},
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
        });
        if crate::successor_interaction::supported(&interaction.method) {
            normalized["controller_generation"] = serde_json::json!(
                self.acceptance_controller
                    .as_ref()
                    .map_or(1, |c| c.generation)
            );
        }
        Ok(normalized)
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

    pub(crate) fn replay_public_response(
        &self,
        intent: &crate::public_resolution::Intent,
        key: &str,
    ) -> Result<Option<Uuid>, TurnError> {
        let controller = self
            .acceptance_controller
            .as_ref()
            .ok_or(TurnError::InvalidInput("public Controller is missing"))?;
        let workspace = self
            .journal
            .workspace_ref()
            .ok_or(TurnError::InvalidInput("public workspace is missing"))?;
        let run = self.run_id.to_string();
        let ns = crate::operation::Namespace::delegation(
            controller,
            workspace,
            Some(("run", &run)),
            "resolve_interaction",
            key,
        )
        .map_err(TurnError::InvalidInput)?;
        if let Some(receipt) = self
            .public_resolutions
            .iter()
            .find(|r| r.mutation_envelope.namespace == ns)
        {
            if receipt.accepted_subjects.interaction_ref.as_deref()
                != Some(intent.body.interaction_ref.as_str())
                || receipt.accepted_subjects.run_ref.as_deref() != Some(run.as_str())
            {
                let target = self
                    .public_request_ids
                    .iter()
                    .find_map(|(upstream, id)| {
                        (id.to_string() == intent.body.interaction_ref)
                            .then(|| self.pending.get(upstream))
                            .flatten()
                    })
                    .ok_or(TurnError::InvalidInput("public interaction is not current"))?;
                if intent.body.run_ref != run
                    || intent.body.turn_ref != target.turn_id
                    || intent.body.thread_generation != self.thread_generation
                {
                    return Err(TurnError::InteractionStale {
                        request_id: target.request_id,
                    });
                }
                let protected = crate::successor_interaction::supported(&target.method)
                    || target.method == "item/tool/requestUserInput"
                        && target.payload["questions"]
                            .as_array()
                            .is_some_and(|q| q.iter().any(|q| q["isSecret"] == true));
                let mut safe = intent.clone();
                safe.redact(protected);
                let candidate = safe.receipt(ns, Uuid::now_v7(), protected)?;
                return Err(TurnError::IdempotencyConflict {
                    recorded: receipt
                        .accepted_input_digest
                        .trim_start_matches("sha256:")
                        .into(),
                    observed: candidate
                        .accepted_input_digest
                        .trim_start_matches("sha256:")
                        .into(),
                });
            }
            // The retained receipt records the authoritative protection choice.
            // Its original receipt wins without looking at replacement content.
            if receipt.contains_protected_input {
                return Ok(Some(receipt.operation_ref));
            }
            let candidate = intent.receipt(ns, receipt.operation_ref, false)?;
            if candidate.accepted_input_digest != receipt.accepted_input_digest {
                return Err(TurnError::IdempotencyConflict {
                    recorded: receipt
                        .accepted_input_digest
                        .trim_start_matches("sha256:")
                        .into(),
                    observed: candidate
                        .accepted_input_digest
                        .trim_start_matches("sha256:")
                        .into(),
                });
            }
            return Ok(Some(receipt.operation_ref));
        }
        Ok(None)
    }

    pub(crate) fn respond_public(
        &mut self,
        request_id: u64,
        key: String,
        response: Value,
        intent: crate::public_resolution::Intent,
    ) -> Result<Option<Uuid>, TurnError> {
        if let Some(receipt) = self.replay_public_response(&intent, &key)? {
            return Ok(Some(receipt));
        }
        let pending = self
            .pending
            .get(&request_id)
            .ok_or(TurnError::InteractionNotFound { request_id })?;
        let protected = crate::successor_interaction::supported(&pending.method)
            || pending.method == "item/tool/requestUserInput"
                && pending.payload["questions"]
                    .as_array()
                    .is_some_and(|q| q.iter().any(|q| q["isSecret"] == true));
        let b = &intent.body;
        if self
            .public_request_ids
            .get(&request_id)
            .map(Uuid::to_string)
            .as_deref()
            != Some(b.interaction_ref.as_str())
            || b.run_ref != self.run_id.to_string()
            || b.turn_ref != pending.turn_id
            || b.thread_generation != self.thread_generation
            || (!protected && b.protected_input_present)
            || (protected && (!b.answers.is_empty() || !b.granted_permissions.is_empty()))
        {
            return Err(TurnError::InteractionStale { request_id });
        }
        if pending.method == "item/tool/requestUserInput" && b.decision != "accept_once" {
            return Err(TurnError::InteractionResponseInvalid { request_id });
        }
        if !protected {
            let expected = if pending.method == "item/tool/requestUserInput" {
                let ids = pending.payload["questions"]
                    .as_array()
                    .ok_or(TurnError::InteractionResponseInvalid { request_id })?;
                if ids.len() != b.answers.len()
                    || ids
                        .iter()
                        .zip(&b.answers)
                        .any(|(q, a)| q["id"] != a.question_id)
                {
                    return Err(TurnError::InteractionResponseInvalid { request_id });
                }
                let answers = b
                    .answers
                    .iter()
                    .map(|a| {
                        let mut values = a.selected_labels.clone();
                        if let Some(text) = &a.text {
                            values.push(text.clone());
                        }
                        (a.question_id.clone(), serde_json::json!({"answers":values}))
                    })
                    .collect::<serde_json::Map<_, _>>();
                serde_json::json!({"answers":answers})
            } else {
                serde_json::json!({"decision":b.decision})
            };
            if expected != response || !b.granted_permissions.is_empty() {
                return Err(TurnError::InteractionResponseInvalid { request_id });
            }
        } else if response["decision"]
            .as_str()
            .is_some_and(|d| d != b.decision)
        {
            return Err(TurnError::InteractionResponseInvalid { request_id });
        }
        let controller = self
            .acceptance_controller
            .as_ref()
            .ok_or(TurnError::InvalidInput("public Controller is missing"))?;
        let run = self.run_id.to_string();
        let namespace = crate::operation::Namespace::delegation(
            controller,
            self.journal
                .workspace_ref()
                .ok_or(TurnError::InvalidInput("public workspace is missing"))?,
            Some(("run", &run)),
            "resolve_interaction",
            &key,
        )
        .map_err(TurnError::InvalidInput)?;
        let receipt = intent.receipt(namespace, Uuid::now_v7(), protected)?;
        self.respond_recorded(request_id, key, response, None, Some(receipt))
    }

    pub fn respond(
        &mut self,
        request_id: u64,
        idempotency_key: String,
        response: Value,
    ) -> Result<Option<Uuid>, TurnError> {
        self.respond_with_policy(request_id, idempotency_key, response, None)
    }

    fn respond_with_policy(
        &mut self,
        request_id: u64,
        idempotency_key: String,
        response: Value,
        policy_cause: Option<&str>,
    ) -> Result<Option<Uuid>, TurnError> {
        self.respond_recorded(request_id, idempotency_key, response, policy_cause, None)
    }

    fn respond_recorded(
        &mut self,
        request_id: u64,
        idempotency_key: String,
        response: Value,
        policy_cause: Option<&str>,
        public_resolution: Option<crate::public_resolution::Receipt>,
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
        let successor = crate::successor_interaction::supported(&interaction.method);
        let (mut wire_response, contains_secret) = if successor {
            if interaction.thread_id != self.thread_id.as_deref().unwrap_or("")
                || interaction.turn_id != self.active_turn_id.as_deref().unwrap_or("")
            {
                return Err(TurnError::InteractionStale { request_id });
            }
            (
                crate::successor_interaction::response(
                    &interaction.method,
                    &interaction.payload,
                    &response.0,
                    &self.cwd,
                    self.writer_generation.is_some(),
                    request_id,
                )?,
                true,
            )
        } else {
            validate_interaction_response(&interaction, &response.0)?
        };
        let response_sha256 = (!contains_secret)
            .then(|| interaction_response_digest(&response.0, request_id))
            .transpose()?;
        let resolution_receipt_id = Some(
            public_resolution
                .as_ref()
                .map_or_else(Uuid::now_v7, |r| r.operation_ref),
        );
        let resolution = if successor {
            serde_json::json!({"outcome":"protected_decision","decision":response.0["decision"],"contained_secret":true,"answer_count":response.0.get("answers").and_then(Value::as_object).map_or(0,serde_json::Map::len),"resolution_receipt_id":resolution_receipt_id,"policy_cause":policy_cause})
        } else if interaction.method == "item/tool/requestUserInput" {
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
                public_resolution: public_resolution.clone().map(Box::new),
                resolved_at: SystemLedgerClock::default().timestamp(),
                resolution,
            })?;
        if let Some(receipt) = public_resolution {
            self.public_resolutions.push(receipt);
        }
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

    fn runtime_error_activity(&mut self) -> Result<(), TurnError> {
        if let (Some(thread_id), Some(turn_id)) = (&self.thread_id, &self.active_turn_id) {
            self.journal.append_and_sync(JournalEntry::SafeActivity {
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
                data: crate::event::ClientEventData::RuntimeError(
                    crate::event::RuntimeErrorPayload {
                        error_code: "RUNTIME_ERROR".into(),
                        message: "The execution runtime reported a failure.".into(),
                    },
                ),
            })?;
        }
        Ok(())
    }

    fn capture_safe_activity(&mut self, params: &Value, completed: bool) -> Result<(), TurnError> {
        if params.get("threadId").and_then(Value::as_str) != self.thread_id.as_deref() {
            return Ok(());
        }
        let turn_id = event_turn_id(params).ok_or(TurnError::CorrelationMismatch)?;
        if self.active_turn_id.as_deref() != Some(turn_id) {
            return Err(TurnError::CorrelationMismatch);
        }
        let item = &params["item"];
        let Some(kind) = item["type"].as_str() else {
            return Ok(());
        };
        if !matches!(kind, "commandExecution" | "fileChange") {
            return Ok(());
        }
        let id = item["id"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 256)
            .ok_or(TurnError::CorrelationMismatch)?;
        if self.activity_items.len() >= MAX_TURN_ITEMS * 2 {
            return Err(TurnError::CorrelationMismatch);
        }
        if !self
            .activity_items
            .insert((turn_id.to_owned(), id.to_owned(), completed))
        {
            return Ok(());
        }
        let data = if kind == "commandExecution" {
            if completed {
                crate::event::ClientEventData::CommandCompleted(
                    crate::event::CommandCompletedPayload {
                        command: vec!["[redacted]".into()],
                        exit_status: item.get("exitCode").and_then(Value::as_i64),
                    },
                )
            } else {
                crate::event::ClientEventData::CommandStarted(crate::event::CommandStartedPayload {
                    command: vec!["[redacted]".into()],
                })
            }
        } else if completed {
            let snapshot = self
                .file_change_snapshots
                .get(&(
                    self.thread_id
                        .clone()
                        .ok_or(TurnError::CorrelationMismatch)?,
                    turn_id.to_owned(),
                    id.to_owned(),
                ))
                .ok_or(TurnError::CorrelationMismatch)?;
            crate::event::ClientEventData::WorkspaceChanges(crate::event::WorkspaceChangesPayload {
                paths: snapshot.changes.iter().map(|c| c.path.clone()).collect(),
                truncated: false,
            })
        } else {
            return Ok(());
        };
        self.journal.append_and_sync(JournalEntry::SafeActivity {
            thread_id: self
                .thread_id
                .clone()
                .ok_or(TurnError::CorrelationMismatch)?,
            turn_id: turn_id.to_owned(),
            data,
        })
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
        self.observe_candidate_item(params, true)?;
        self.capture_safe_activity(params, true)?;
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
        if status == "failed" {
            self.runtime_error_activity()?;
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
        let successor_checked =
            self.defer_checked_history_publication && self.candidate_reader.is_some();
        if !successor_checked && let Some(reader) = &self.candidate_reader {
            reader
                .begin_history(&thread_id, &turn_id)
                .map_err(|_| TurnError::CorrelationMismatch)?;
        }
        let inline_response = if successor_checked {
            None
        } else {
            inline_items
                .as_ref()
                .map(|items| self.extract_final_response(&thread_id, &turn_id, items))
        };
        for request_id in self.pending.keys().copied().collect::<Vec<_>>() {
            self.journal
                .append_and_sync(JournalEntry::ApprovalDecided {
                    request_id,
                    idempotency_key: String::new(),
                    response_sha256: None,
                    resolution_receipt_id: None,
                    public_resolution: None,
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
        // Successor status and usage are durable before reader storage is
        // touched; their response waits for verified history. Other callers
        // retain their existing notification receipt and history ordering.
        self.pending_terminal_history = if successor_checked {
            None
        } else {
            inline_items
                .as_ref()
                .filter(|_| self.candidate_reader.is_some())
                .map(|items| RecoveredHistory {
                    thread_id: thread_id.clone(),
                    turn_id: turn_id.clone(),
                    status: status.clone(),
                    full: true,
                    items: items.clone(),
                    usage: usage.clone(),
                })
        };
        let final_response = if self.candidate_reader.is_some() {
            if successor_checked {
                self.candidate_reader
                    .as_ref()
                    .expect("checked reader")
                    .begin_history(&thread_id, &turn_id)
                    .map_err(|_| TurnError::CorrelationMismatch)?;
            }
            let items = self.streamed_items(&thread_id, &turn_id)?;
            self.pending_terminal_history = Some(RecoveredHistory {
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
                status: status.clone(),
                full: true,
                items: items.clone(),
                usage: usage.clone(),
            });
            let response = self.extract_final_response(&thread_id, &turn_id, &items)?;
            self.candidate_reader
                .as_ref()
                .expect("checked reader")
                .finish_history_response(&thread_id, &turn_id, &items, response.clone())
                .map_err(|_| TurnError::CorrelationMismatch)?;
            response
        } else {
            match inline_response {
                Some(response) => response?,
                None => {
                    let items = self.streamed_items(&thread_id, &turn_id)?;
                    self.extract_final_response(&thread_id, &turn_id, &items)?
                }
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
        self.clear_pending_terminal_history();
        Ok(terminal)
    }

    /// The Turn's authoritative items, read from the Thread's own history.
    ///
    /// Only reached when `turn/completed` omitted them: the notification's
    /// items are authoritative when present, and reading history again would
    /// cost a round trip for an answer already in hand.
    fn streamed_items(&mut self, thread_id: &str, turn_id: &str) -> Result<Vec<Value>, TurnError> {
        let mut sink = if self.candidate_tools {
            ThreadTurnSink::new_bound(thread_id, turn_id)
        } else {
            ThreadTurnSink::new(turn_id)
        };
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
        if let Some(reader) = &self.candidate_reader {
            for item in authoritative_items {
                if let Err(error) = reader.observe_item(thread_id, turn_id, item, true) {
                    self.candidate_history_failure = Some(error);
                    return Err(TurnError::CorrelationMismatch);
                }
            }
        }
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
            Err(error) => Ok(Some(FinalResponse::Unavailable {
                byte_length,
                sha256,
                reason: if matches!(error, TurnError::ArtifactQuotaExceeded) {
                    "quota_exceeded"
                } else {
                    "write_failed"
                }
                .to_owned(),
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
    refused: BTreeMap<String, String>,
}

pub(crate) fn validate_submit_history(records: Vec<(AuditKind, Value)>) -> Result<(), TurnError> {
    restore_turn_receipt_maps(records).map(|_| ())
}

pub(crate) fn submit_audit_history_is_quiescent(
    records: &[crate::audit::AuditRecord],
) -> Result<bool, TurnError> {
    let payloads = records
        .iter()
        .map(|record| {
            let bytes =
                canonicalize(record.payload()).map_err(|_| TurnError::CorrelationMismatch)?;
            Ok((
                record.kind(),
                serde_json::from_slice(&bytes).map_err(|_| TurnError::CorrelationMismatch)?,
            ))
        })
        .collect::<Result<Vec<_>, TurnError>>()?;
    submit_history_is_quiescent(payloads)
}

pub(crate) fn submit_history_is_quiescent(
    records: Vec<(AuditKind, Value)>,
) -> Result<bool, TurnError> {
    restore_turn_receipt_maps(records).map(|restored| restored.unresolved.is_empty())
}

fn restore_turn_receipt_maps(
    records: Vec<(AuditKind, Value)>,
) -> Result<RestoredTurnReceipts, TurnError> {
    // Older workers released a correlated refusal only in memory. Preserve
    // their already accepted retry, but never use an unresolved legacy repeat
    // as permission to dispatch or relax successor operation fences.
    let successor_operations: BTreeSet<_> = records
        .iter()
        .filter(|(kind, payload)| {
            *kind == AuditKind::TurnIntent && payload.get("schema_version").is_some()
        })
        .filter_map(|(_, payload)| payload.get("operation_ref")?.as_str().map(str::to_owned))
        .collect();
    let legacy_identity = |intent: &OperationIntent| {
        (
            intent.idempotency_key.clone(),
            intent.request_sha256.clone(),
            intent.run_generation,
            intent.server_key.clone(),
            intent.server_epoch,
        )
    };
    let mut legacy_acceptances = BTreeMap::new();
    let mut legacy_threads = BTreeSet::new();
    for (index, (kind, payload)) in records.iter().enumerate() {
        if *kind == AuditKind::ThreadBound {
            if let Some(thread) = payload.get("thread_id").and_then(Value::as_str) {
                legacy_threads.insert(thread.to_owned());
            }
        } else if *kind == AuditKind::TurnStarted
            && payload
                .get("thread_id")
                .and_then(Value::as_str)
                .is_some_and(|thread| legacy_threads.contains(thread))
            && payload
                .get("turn_id")
                .and_then(Value::as_str)
                .is_some_and(|turn| !turn.is_empty())
            && let Some(operation) = payload.get("operation_id").and_then(Value::as_str)
            && !successor_operations.contains(operation)
        {
            legacy_acceptances
                .entry(operation.to_owned())
                .or_insert(index);
        }
    }
    let mut accepted_legacy_chains = BTreeMap::new();
    for (index, (kind, payload)) in records.iter().enumerate() {
        if *kind == AuditKind::IdempotencyReserved && payload.get("operation_id").is_some() {
            let intent: OperationIntent = serde_json::from_value(payload.clone())
                .map_err(|_| TurnError::Journal("invalid durable Turn intent".to_owned()))?;
            if let Some(&acceptance_index) =
                legacy_acceptances.get(&intent.operation_id.to_string())
                && index < acceptance_index
            {
                accepted_legacy_chains
                    .entry(legacy_identity(&intent))
                    .and_modify(|latest: &mut usize| *latest = (*latest).max(acceptance_index))
                    .or_insert(acceptance_index);
            }
        }
    }
    let mut idempotency = BTreeMap::new();
    let mut unresolved_idempotency = BTreeMap::new();
    let mut refused_idempotency = BTreeMap::new();
    let mut current_operations = BTreeMap::new();
    let mut bound_threads = BTreeSet::new();
    let mut intents = BTreeMap::<String, OperationIntent>::new();
    let mut accepted = BTreeMap::<String, (String, String)>::new();
    for (index, (kind, payload)) in records.into_iter().enumerate() {
        match kind {
            AuditKind::IdempotencyReserved if payload.get("operation_id").is_some() => {
                let intent: OperationIntent = serde_json::from_value(payload)
                    .map_err(|_| TurnError::Journal("invalid durable Turn intent".to_owned()))?;
                let accepted_legacy_retry = current_operations
                    .get(&intent.idempotency_key)
                    .and_then(|previous: &Uuid| intents.get(&previous.to_string()))
                    .is_some_and(|previous| {
                        previous.operation_id != intent.operation_id
                            && previous.request_sha256 == intent.request_sha256
                            && previous.run_generation == intent.run_generation
                            && previous.server_key == intent.server_key
                            && previous.server_epoch == intent.server_epoch
                            && !successor_operations.contains(&previous.operation_id.to_string())
                            && !successor_operations.contains(&intent.operation_id.to_string())
                            && !legacy_acceptances.contains_key(&previous.operation_id.to_string())
                            && accepted_legacy_chains
                                .get(&legacy_identity(&intent))
                                .is_some_and(|acceptance_index| index < *acceptance_index)
                    });
                if (unresolved_idempotency.contains_key(&intent.idempotency_key)
                    && !accepted_legacy_retry)
                    || idempotency.contains_key(&intent.idempotency_key)
                    || refused_idempotency
                        .get(&intent.idempotency_key)
                        .is_some_and(|digest| digest != &intent.request_sha256)
                {
                    return Err(TurnError::Journal(
                        "durable Turn key was redispatched unsafely".to_owned(),
                    ));
                }
                current_operations.insert(intent.idempotency_key.clone(), intent.operation_id);
                unresolved_idempotency.insert(
                    intent.idempotency_key.clone(),
                    intent.request_sha256.clone(),
                );
                intents.insert(intent.operation_id.to_string(), intent);
            }
            AuditKind::TurnIntent
                if payload.get("schema").and_then(Value::as_str)
                    == Some("dolgorae.submit-refusal/v1") =>
            {
                let refusal: crate::operation::SubmitRefusal = serde_json::from_value(payload)
                    .map_err(|_| TurnError::Journal("invalid durable Submit refusal".to_owned()))?;
                let intent = intents
                    .get(&refusal.operation_id.to_string())
                    .ok_or_else(|| {
                        TurnError::Journal("Submit refusal has no original intent".to_owned())
                    })?;
                refusal.validate_intent(intent)?;
                if current_operations.get(&refusal.idempotency_key) != Some(&refusal.operation_id)
                    || !unresolved_idempotency.contains_key(&refusal.idempotency_key)
                {
                    return Err(TurnError::Journal(
                        "Submit refusal crossed an operation boundary".to_owned(),
                    ));
                }
                unresolved_idempotency.remove(&refusal.idempotency_key);
                refused_idempotency.insert(refusal.idempotency_key, refusal.request_sha256);
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
                    if current_operations.get(&intent.idempotency_key) != Some(&intent.operation_id)
                        || unresolved_idempotency.get(&intent.idempotency_key)
                            != Some(&intent.request_sha256)
                    {
                        return Err(TurnError::Journal(
                            "acceptance crossed a resolved operation boundary".to_owned(),
                        ));
                    }
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
                        refused_idempotency.remove(&intent.idempotency_key);
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
        refused: refused_idempotency,
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
        refused,
    } = restore_turn_receipt_maps(records)?;
    let effort = request.effort.as_deref().unwrap_or(default_effort);
    let digest = request_digest(request, effort)?;
    if let Some(recorded) = refused.get(&request.idempotency_key)
        && recorded != &digest
    {
        return Err(TurnError::IdempotencyConflict {
            recorded: recorded.clone(),
            observed: digest,
        });
    }
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

/// Positive original acceptance only. A missing or unresolved receipt is never
/// proof that a reserved request cannot still reach the Worker.
pub(crate) fn verified_submit_acceptance(
    records: &[crate::audit::AuditRecord],
    namespace: &crate::operation::Namespace,
    request: &TurnRequest,
) -> Result<Option<(AcceptedTurn, u64)>, TurnError> {
    let invalid = || TurnError::Journal("original Submit acceptance is inconsistent".into());
    let lookup = crate::operation::lookup(records, namespace)?;
    if lookup["state"] != "accepted" {
        return Ok(None);
    }
    let operation = lookup["operation_ref"].as_str().ok_or_else(invalid)?;
    let sequence = lookup["receipt"]["accepted_revision"]
        .as_u64()
        .ok_or_else(invalid)?;
    let record = records
        .iter()
        .find(|r| r.sequence() == sequence)
        .ok_or_else(invalid)?;
    let value: Value =
        serde_json::from_slice(&canonicalize(record.payload()).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    if record.kind() != AuditKind::TurnStarted || value["operation_id"] != operation {
        return Err(invalid());
    }
    let effort = value["effort"].as_str().ok_or_else(invalid)?;
    let expected = crate::operation::SubmitIdentity::prepare(
        Uuid::parse_str(operation).map_err(|_| invalid())?,
        namespace.clone(),
        request,
        effort,
    )?;
    if lookup["accepted_input_digest"] != expected.mutation_envelope_digest {
        return Err(invalid());
    }
    let mut history = Vec::with_capacity(records.len());
    let mut intent = None;
    for item in records {
        let payload: Value =
            serde_json::from_slice(&canonicalize(item.payload()).map_err(|_| invalid())?)
                .map_err(|_| invalid())?;
        if item.kind() == AuditKind::IdempotencyReserved && payload["operation_id"] == operation {
            if intent.is_some() {
                return Err(invalid());
            }
            let original: OperationIntent =
                serde_json::from_value(payload.clone()).map_err(|_| invalid())?;
            if original.run_generation == 0
                || original.run_generation != record.run_generation()
                || item.run_generation() != original.run_generation
                || item.run_id() != record.run_id()
                || original.server_epoch == 0
                || original.server_key.is_empty()
            {
                return Err(invalid());
            }
            intent = Some(original);
        }
        history.push((item.kind(), payload));
    }
    let original = intent.ok_or_else(invalid)?;
    let mut request = request.clone();
    request.effort = Some(effort.to_owned());
    let accepted = replay_durable_submit(history, &request, effort)?.ok_or_else(invalid)?;
    if lookup["accepted_subjects"]["turn_ref"] != accepted.turn_id
        || value["thread_id"] != accepted.thread_id
        || original.idempotency_key != namespace.idempotency_key
    {
        return Err(invalid());
    }
    Ok(Some((accepted, original.run_generation)))
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

/// Publish one normalized file-change payload atomically and write-once (MD1).
///
/// The bytes are content-addressed by their digest: an identical republication is
/// a no-op, and a different payload under the same digest is an integrity failure,
/// never a replacement.
fn publish_file_change_payload(
    directory: &Path,
    snapshot_sha256: &str,
    bytes: &[u8],
) -> Result<(), TurnError> {
    std::fs::create_dir_all(directory).map_err(|error| TurnError::Journal(error.to_string()))?;
    std::fs::set_permissions(
        directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .map_err(|error| TurnError::Journal(error.to_string()))?;
    let path = directory.join(format!("{snapshot_sha256}.json"));
    match std::fs::read(&path) {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => {
            return Err(TurnError::Journal(
                "the published file-change payload is another one".to_owned(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(TurnError::Journal(
                "the published file-change payload is unreadable".to_owned(),
            ));
        }
    }
    match crate::workspace::atomic_create(
        &crate::workspace::SystemWorkspacePlatform,
        &path,
        bytes,
        0o600,
    ) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::read(&path) {
                Ok(existing) if existing == bytes => Ok(()),
                _ => Err(TurnError::Journal(
                    "the published file-change payload is another one".to_owned(),
                )),
            }
        }
        Err(error) => Err(TurnError::Journal(error.to_string())),
    }
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
                workspace,
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
                        .and_then(|path| checked_change_path(path, workspace))
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

fn checked_change_path(path: &str, workspace: &Path) -> Result<String, TurnError> {
    use std::path::Component;
    if path.is_empty() || path.len() > 4_096 {
        return Err(TurnError::CorrelationMismatch);
    }
    let path = Path::new(path);
    let relative = if path.is_absolute() {
        path.strip_prefix(workspace)
            .map_err(|_| TurnError::CorrelationMismatch)?
    } else {
        path
    };
    if !relative
        .components()
        .any(|part| matches!(part, Component::Normal(_)))
        || relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(TurnError::CorrelationMismatch);
    }
    relative
        .to_str()
        .map(str::to_owned)
        .ok_or(TurnError::CorrelationMismatch)
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

pub(crate) fn image_media_type(path: &LosslessPath) -> Result<String, TurnError> {
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
pub(crate) mod tests {
    /// Exercise the normal notification ingest with the worker-selected path.
    pub(crate) fn fixture_file_change_publication(directory: Option<PathBuf>) -> String {
        let mut coordinator = running_turn_with(
            &RecordingDiagnostics::default(),
            vec![
                serde_json::json!({"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1",
                "item":{"id":"change-1","type":"fileChange","changes":[{"path":"ignored.log","kind":{"type":"add"},"diff":"@@ -0,0 +1 @@\n+recorded\n"}]}}}),
            ],
        );
        coordinator.file_change_publication_dir = directory;
        coordinator.next_event().unwrap();
        coordinator
            .journal
            .entries
            .iter()
            .find_map(|entry| match entry {
                JournalEntry::FileChangeSnapshot {
                    snapshot_sha256, ..
                } => Some(snapshot_sha256.clone()),
                _ => None,
            })
            .expect("ingest journals the normalized item")
    }
    #[test]
    fn a_successor_bound_turn_publishes_its_file_change_payload_once() {
        let root =
            std::env::temp_dir().join(format!("d74-file-changes-{}", Uuid::now_v7().simple()));
        let directory = root.join("file-changes");
        let bytes = br#"[{"path":"ignored.log","kind":"update"}]"#;
        let digest = sha256_hex(bytes);
        publish_file_change_payload(&directory, &digest, bytes).unwrap();
        let path = directory.join(format!("{digest}.json"));
        assert!(path.exists());
        // The directory and the payload are private.
        assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        // An identical republication is a no-op.
        publish_file_change_payload(&directory, &digest, bytes).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // A different payload under the same digest is an integrity failure, never a
        // replacement.
        assert!(matches!(
            publish_file_change_payload(&directory, &digest, b"[]"),
            Err(TurnError::Journal(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // A legacy Turn (no publication directory configured) publishes nothing.
        let legacy = root.join("legacy");
        assert!(!legacy.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    use super::*;
    use crate::primary_tool::PRIMARY_TOOL_NAME;
    use std::collections::VecDeque;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    pub(crate) fn native_worker_completion(
        state_root: &Path,
        root: Uuid,
        workspace_id: &str,
        workspace: &Path,
        tool: &str,
        payload: Value,
    ) -> Result<(Value, Value, Vec<u8>), TurnError> {
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut replies = Vec::new();
        for (id, result) in [
            (1, serde_json::json!({"codexHome":"/tmp/fixture"})),
            (2, serde_json::json!({"thread":{"id":"native-thread"}})),
            (3, serde_json::json!({"turn":{"id":"native-turn"}})),
        ] {
            let bytes = serde_json::to_vec(&serde_json::json!({"id":id,"result":result})).unwrap();
            assert!(bytes.len() < 126);
            replies.extend([0x81, bytes.len() as u8]);
            replies.extend(bytes);
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(crate::app_server::SharedWire::new(Capture(Arc::clone(
            &output,
        ))));
        let wire = crate::app_server::SplitWire::new(std::io::Cursor::new(replies), shared);
        let worker =
            crate::worker::tests::PrimaryWorkerFixture::new(state_root, root, workspace_id);
        let mut coordinator = TurnCoordinator::initialize(
            JsonRpcConnection::new(wire),
            MemoryJournal::default(),
            MemoryArtifactStore::default(),
            CoordinatorConfig {
                attach: ThreadAttach::Start,
                foreign_diagnostics: Box::new(RecordingDiagnostics::default()),
                fixed_model: "gpt-5".into(),
                default_effort: "medium".into(),
                supported_efforts: ["medium".into()].into(),
                cwd: workspace.into(),
                developer_instructions: String::new(),
                sandbox: "read-only".into(),
                approval_policy: "never".into(),
                safety_policy: SessionSafetyPolicy::Standard,
                run_generation: 1,
                server_key: "a".repeat(64),
                server_epoch: 1,
                run_id: root,
                controller_id: Uuid::now_v7(),
                control_mode: "managed_agent".into(),
                record_accepted_user_input: false,
                primary_tool: Some(PrimaryToolConfig {
                    session_id: if tool == crate::collaboration::TOOL_NAME {
                        crate::run::RunStore::new(
                            crate::workspace::SystemWorkspacePlatform,
                            state_root,
                        )
                        .load_manifest(root)
                        .unwrap()
                        .aggregate_binding
                        .unwrap()
                        .aggregate_id
                    } else {
                        root
                    },
                    contract: if tool == crate::collaboration::TOOL_NAME {
                        PrimaryToolContract::load().unwrap().for_collaboration()
                    } else {
                        PrimaryToolContract::load().unwrap().with_specialists()
                    },
                    dispatcher: Arc::clone(&worker.dispatcher),
                }),
                candidate_tools: false,
                candidate_reader: None,
                file_change_publication_dir: None,
            },
            "/tmp/fixture",
        )?;
        coordinator.start_turn(request("native-fixture", DeliveryMode::Submit))?;
        output.lock().unwrap().clear();
        coordinator.ingest(serde_json::json!({"id":73,"method":"item/tool/call","params":{"threadId":"native-thread","turnId":"native-turn","callId":"native-call","tool":tool,"arguments":payload}}))?;
        let completion = worker.completion();
        let original = completion.result.clone();
        coordinator.complete_primary_tool(completion)?;
        let bytes = output.lock().unwrap().clone();
        let messages = crate::app_server::tests::client_messages(&bytes);
        assert_eq!(messages.len(), 1);
        let envelope: Value = serde_json::from_slice(&messages[0]).unwrap();
        assert_eq!(envelope["id"], 73);
        Ok((
            match original {
                Ok(body) => body,
                Err(error) => coordinator
                    .primary_tool
                    .as_ref()
                    .unwrap()
                    .contract
                    .error_result(&error),
            },
            envelope["result"].clone(),
            bytes,
        ))
    }

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
        experimental_api: bool,
    }
    impl AppServer for FakeServer {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, TurnError> {
            if method == "initialize" {
                self.experimental_api =
                    params.pointer("/capabilities/experimentalApi") == Some(&Value::Bool(true));
            }
            self.calls.push((method.to_owned(), params));
            if method == "thread/settings/update" && !self.experimental_api {
                return Err(TurnError::AppServerRejected {
                    code: -32600,
                    message: "experimental API not enabled".to_owned(),
                });
            }
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
        server: FakeServer,
        attach: ThreadAttach,
        foreign_diagnostics: Box<dyn ForeignDiagnostics>,
        safety_policy: SessionSafetyPolicy,
        primary_tool: Option<PrimaryToolConfig>,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        coordinator_with_journal(
            server,
            attach,
            foreign_diagnostics,
            safety_policy,
            primary_tool,
            MemoryJournal::default(),
        )
    }

    fn coordinator_with_journal<J: DurableTurnJournal>(
        server: FakeServer,
        attach: ThreadAttach,
        foreign_diagnostics: Box<dyn ForeignDiagnostics>,
        safety_policy: SessionSafetyPolicy,
        primary_tool: Option<PrimaryToolConfig>,
        journal: J,
    ) -> TurnCoordinator<FakeServer, J, MemoryArtifactStore> {
        coordinator_with_journal_and_artifacts(
            server,
            attach,
            foreign_diagnostics,
            safety_policy,
            primary_tool,
            journal,
            MemoryArtifactStore::default(),
        )
    }

    fn coordinator_with_journal_and_artifacts<J: DurableTurnJournal, A: ResponseArtifactStore>(
        mut server: FakeServer,
        attach: ThreadAttach,
        foreign_diagnostics: Box<dyn ForeignDiagnostics>,
        safety_policy: SessionSafetyPolicy,
        primary_tool: Option<PrimaryToolConfig>,
        journal: J,
        artifacts: A,
    ) -> TurnCoordinator<FakeServer, J, A> {
        server
            .replies
            .push_front(Ok(serde_json::json!({"codexHome":"/tmp/codex-home"})));
        TurnCoordinator::initialize(
            server,
            journal,
            artifacts,
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
                candidate_tools: false,
                candidate_reader: None,
                file_change_publication_dir: None,
            },
            "/tmp/codex-home",
        )
        .unwrap()
    }

    /// Generate original Submit/terminal evidence through the production
    /// coordinator with the existing scripted server, without a native process.
    pub(crate) fn collaboration_submit_history(
        run: Uuid,
        controller: crate::domain::ControllerIdentity,
        workspace: &str,
        request: TurnRequest,
    ) -> (AcceptedTurn, Vec<crate::audit::AuditRecord>) {
        struct ScopedJournal {
            inner: MemoryJournal,
            workspace: String,
        }
        impl DurableTurnJournal for ScopedJournal {
            fn append_and_sync(&mut self, entry: JournalEntry) -> Result<(), TurnError> {
                self.inner.append_and_sync(entry)
            }
            fn workspace_ref(&self) -> Option<&str> {
                Some(&self.workspace)
            }
        }
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"native-thread"}})));
        server.replies.push_back(Ok(
            serde_json::json!({"turn":{"id":"recovered-consultation-turn"}}),
        ));
        let mut c = coordinator_with_journal(
            server,
            ThreadAttach::Start,
            Box::new(RecordingDiagnostics::default()),
            SessionSafetyPolicy::Standard,
            None,
            ScopedJournal {
                inner: MemoryJournal::default(),
                workspace: workspace.into(),
            },
        );
        c.run_id = run;
        c.controller_id = controller.controller_id;
        c.fixed_model = request.model.clone();
        c.record_accepted_user_input = false;
        c.set_acceptance_controller(controller);
        let accepted = c.start_turn(request).unwrap();
        c.ingest(serde_json::json!({"method":"turn/completed","params":{"threadId":accepted.thread_id,"turn":{"id":accepted.turn_id,"status":"completed","items":[{"type":"agentMessage","phase":"final_answer","text":"Recovered original consultation answer."}]}}})).unwrap();
        let mut records = Vec::new();
        let mut previous = crate::audit::GENESIS_PREVIOUS_HASH.to_owned();
        for entry in c.journal().inner.entries.iter().cloned() {
            let (kind, payload) = journal_record(entry).unwrap();
            let record = crate::audit::AuditRecord::new(
                records.len() as u64 + 1,
                SystemLedgerClock::default().timestamp(),
                run,
                1,
                kind,
                crate::jcs::parse(&payload.to_string()).unwrap(),
                &previous,
            )
            .unwrap();
            previous = record.hash().into();
            records.push(record);
        }
        (accepted, records)
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

    fn direct_coordinator(
        reader: crate::direct_review_target::DirectTargetReader,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        candidate_coordinator(
            reader.workspace().unwrap(),
            reader.run_id(),
            true,
            Some(reader),
        )
    }

    fn candidate_coordinator(
        workspace: PathBuf,
        run_id: Uuid,
        candidate_tools: bool,
        reader: Option<crate::direct_review_target::DirectTargetReader>,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        let mut server = FakeServer::default();
        for reply in [
            serde_json::json!({"codexHome":"/tmp/codex-home"}),
            serde_json::json!({"cwd":workspace.clone(),"approvalPolicy":"never","sandbox":{"type":"readOnly"},"model":"gpt-5","thread":{"id":"thread-1","parentThreadId":null,"turns":[]}}),
            serde_json::json!({}),
            serde_json::json!({"thread":{"id":"thread-1","parentThreadId":null,"turns":[]}}),
            serde_json::json!({"turn":{"id":"turn-1"}}),
        ] {
            server.replies.push_back(Ok(reply));
        }
        TurnCoordinator::initialize(
            server,
            MemoryJournal::default(),
            MemoryArtifactStore::default(),
            CoordinatorConfig {
                attach: ThreadAttach::Start,
                foreign_diagnostics: Box::new(RecordingDiagnostics::default()),
                fixed_model: "gpt-5".to_owned(),
                default_effort: "medium".to_owned(),
                supported_efforts: ["medium".to_owned()].into_iter().collect(),
                cwd: workspace.clone(),
                developer_instructions: "checked reviewer".to_owned(),
                sandbox: "read-only".to_owned(),
                approval_policy: "never".to_owned(),
                safety_policy: SessionSafetyPolicy::ReviewerReadOnly,
                run_generation: 1,
                server_key: "a".repeat(64),
                server_epoch: 7,
                run_id,
                controller_id: Uuid::now_v7(),
                control_mode: "direct_interactive".to_owned(),
                record_accepted_user_input: false,
                primary_tool: None,
                candidate_tools,
                candidate_reader: reader,
                file_change_publication_dir: None,
            },
            "/tmp/codex-home",
        )
        .unwrap()
    }

    #[test]
    fn collaboration_and_candidate_tools_preserve_registration_and_name_routing() {
        let fixture = crate::direct_review_target::tests::Fixture::new(false);
        std::fs::write(fixture.source.join("file.txt"), b"candidate\n").unwrap();
        let reader = fixture.reader(fixture.admit("workspace", None));
        let mut coordinator = direct_coordinator(reader);
        let dispatcher = Arc::new(RecordingPrimaryDispatcher::default());
        coordinator.primary_tool = Some(PrimaryToolConfig {
            session_id: Uuid::now_v7(),
            contract: PrimaryToolContract::load().unwrap().for_collaboration(),
            dispatcher: dispatcher.clone(),
        });
        assert_eq!(coordinator.primary_session_id(), None);
        coordinator
            .start_turn(request("combined-tools", DeliveryMode::Submit))
            .unwrap();
        let params = &coordinator
            .server
            .calls
            .iter()
            .find(|(method, _)| method == "thread/start")
            .unwrap()
            .1;
        let names: Vec<_> = params["dynamicTools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                crate::collaboration::TOOL_NAME,
                crate::direct_review::CANDIDATE_TOOL_NAME
            ]
        );
        coordinator.ingest(serde_json::json!({"id":51,"method":"item/tool/call","params":{"threadId":"thread-1","turnId":"turn-1","callId":"combined-call","tool":crate::collaboration::TOOL_NAME,"arguments":{"operation":"collect_collaboration_results","limit":1}}})).unwrap();
        let calls = dispatcher.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, crate::collaboration::TOOL_NAME);
        assert_eq!(calls[0].context.source_tool_call_id, "combined-call");
    }

    #[test]
    fn successor_candidate_tools_exist_at_initial_thread_creation() {
        crate::worker::fixture_successor_review_sessions(|workspace, run_id, candidate_tools| {
            assert!(candidate_tools);
            let mut coordinator =
                candidate_coordinator(workspace.to_owned(), run_id, candidate_tools, None);
            coordinator.initialize_thread(Uuid::now_v7()).unwrap();
            let params = &coordinator
                .server
                .calls
                .iter()
                .find(|(method, _)| method == "thread/start")
                .unwrap()
                .1;
            assert_eq!(
                params["dynamicTools"][0]["name"],
                crate::direct_review::CANDIDATE_TOOL_NAME
            );
            assert_eq!(params["approvalPolicy"], "never");
            assert_eq!(params["sandbox"], "read-only");
            assert!(coordinator.candidate_reader.is_none());
            assert!(
                coordinator
                    .start_turn(request("unbound-successor", DeliveryMode::Submit))
                    .is_err()
            );
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
        });
    }

    #[test]
    fn reusable_reviewer_installs_boundary_before_target_and_binds_only_while_idle() {
        let fixture = crate::direct_review_target::tests::Fixture::new(false);
        std::fs::write(fixture.source.join("file.txt"), b"candidate\n").unwrap();
        let reader = fixture.reader(fixture.admit("workspace", None));
        let run_id = reader.run_id();
        let review_id = reader.review_id();
        let mut coordinator = direct_coordinator(reader);
        coordinator.candidate_reader = None;
        assert!(
            coordinator
                .start_turn(request("unbound", DeliveryMode::Submit))
                .is_err()
        );
        assert!(
            !coordinator
                .server
                .calls
                .iter()
                .any(|(method, _)| method == "turn/start")
        );
        coordinator.initialize_thread(Uuid::now_v7()).unwrap();
        assert_eq!(coordinator.state, CoordinatorState::Idle);
        assert!(
            coordinator
                .server
                .calls
                .iter()
                .any(|(method, params)| method == "thread/start"
                    && params["dynamicTools"][0]["name"]
                        == crate::direct_review::CANDIDATE_TOOL_NAME)
        );
        let reader = crate::direct_review_target::DirectTargetReader::open(
            &fixture.state,
            review_id,
            run_id,
        )
        .unwrap();
        coordinator.bind_candidate_reader(reader).unwrap();
        coordinator
            .start_turn(request("bound", DeliveryMode::Submit))
            .unwrap();
        let reader = crate::direct_review_target::DirectTargetReader::open(
            &fixture.state,
            review_id,
            run_id,
        )
        .unwrap();
        assert!(coordinator.bind_candidate_reader(reader).is_err());
        assert!(
            coordinator
                .set_writer_access(true, 1, Uuid::now_v7())
                .is_err()
        );
    }

    fn candidate_call(id: u64, thread: &str) -> Value {
        serde_json::json!({"id":id,"method":"item/tool/call","params":{"threadId":thread,"turnId":"turn-1","callId":"read-call","tool":crate::direct_review::CANDIDATE_TOOL_NAME,"arguments":{"side":"current","path":"file.txt","byte_offset":0,"byte_length":10}}})
    }

    #[test]
    fn direct_reader_refuses_mismatched_policy_cwd_and_nonfresh_thread_before_turn_dispatch() {
        let fixture = crate::direct_review_target::tests::Fixture::new(true);
        fixture.commit("candidate\n");
        for (pointer, value) in [
            ("/cwd", serde_json::json!("/wrong")),
            ("/sandbox/type", serde_json::json!("workspaceWrite")),
            ("/approvalPolicy", serde_json::json!("on-request")),
            ("/model", serde_json::json!("wrong")),
            ("/thread/parentThreadId", serde_json::json!("parent")),
            ("/thread/turns", serde_json::json!([{"id":"old"}])),
        ] {
            let reader = fixture.reader(fixture.admit("head", None));
            let mut coordinator = direct_coordinator(reader);
            let response = coordinator
                .server
                .replies
                .front_mut()
                .unwrap()
                .as_mut()
                .unwrap();
            *response.pointer_mut(pointer).unwrap() = value;
            assert!(
                coordinator
                    .start_turn(request("checked", DeliveryMode::Submit))
                    .is_err(),
                "{pointer}"
            );
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
        }
    }

    #[test]
    fn direct_candidate_tools_are_correlated_and_native_items_poison_acceptance() {
        let fixture = crate::direct_review_target::tests::Fixture::new(true);
        fixture.commit("candidate\n");
        let reader = fixture.reader(fixture.admit("head", None));
        let run = reader.run_id();
        let mut coordinator = direct_coordinator(reader);
        coordinator
            .start_turn(request("review", DeliveryMode::Submit))
            .unwrap();
        let params = &coordinator
            .server
            .calls
            .iter()
            .find(|(method, _)| method == "thread/start")
            .unwrap()
            .1;
        assert_eq!(
            params["dynamicTools"][0]["name"],
            crate::direct_review::CANDIDATE_TOOL_NAME
        );
        assert_eq!(
            coordinator.server.calls[0].1["capabilities"]["experimentalApi"],
            true
        );
        coordinator
            .ingest(candidate_call(1, "foreign-thread"))
            .unwrap();
        assert!(coordinator.server.responses.is_empty());
        coordinator.ingest(candidate_call(2, "thread-1")).unwrap();
        coordinator.ingest(candidate_call(3, "thread-1")).unwrap();
        assert_eq!(
            coordinator.server.responses[0].1,
            coordinator.server.responses[1].1
        );
        assert_eq!(coordinator.server.responses[0].1["success"], true);
        coordinator.ingest(serde_json::json!({"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","item":{"type":"dynamicToolCall","id":"read-call","tool":crate::direct_review::CANDIDATE_TOOL_NAME}}})).unwrap();
        coordinator
            .candidate_reader
            .as_ref()
            .unwrap()
            .check_acceptance()
            .unwrap();
        coordinator.server.replies.push_back(Ok(serde_json::json!({"thread":{"id":"thread-1","turns":[{"id":"turn-1","items":[{"type":"commandExecution","id":"native-read","command":"cat file.txt"},{"type":"agentMessage","text":"clean","phase":"final_answer"}]}]}})));
        let terminal=coordinator.ingest(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[{"type":"commandExecution","id":"native-read","command":"cat file.txt"},{"type":"agentMessage","text":"clean","phase":"final_answer"}]}}})).unwrap().unwrap();
        assert_eq!(terminal.status, "completed");
        assert_eq!(coordinator.state(), &CoordinatorState::Idle);
        assert_eq!(
            crate::direct_review_target::DirectTargetReader::for_run(&fixture.state, run)
                .unwrap()
                .unwrap()
                .check_acceptance()
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
    }

    #[test]
    fn direct_reader_response_loss_retains_the_original_receipt() {
        let fixture = crate::direct_review_target::tests::Fixture::new(true);
        fixture.commit("candidate\n");
        let reader = fixture.reader(fixture.admit("head", None));
        let run = reader.run_id();
        let review = reader.review_id();
        let mut coordinator = direct_coordinator(reader);
        coordinator
            .start_turn(request("original", DeliveryMode::Submit))
            .unwrap();
        coordinator.server.fail_response = true;
        assert!(coordinator.ingest(candidate_call(1, "thread-1")).is_err());
        assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
        let response: Value = serde_json::from_str(
            coordinator.server.responses[0].1["contentItems"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let reopened =
            crate::direct_review_target::DirectTargetReader::for_run(&fixture.state, run)
                .unwrap()
                .unwrap();
        assert_eq!(reopened.review_id(), review);
        let retried = reopened
            .read(
                "thread-1",
                "turn-1",
                "read-call",
                &crate::direct_review_target::CandidateReadRequest {
                    side: "current".to_owned(),
                    path: "file.txt".to_owned(),
                    byte_offset: 0,
                    byte_length: 10,
                },
            )
            .unwrap();
        assert_eq!(retried, response);
        assert_eq!(
            coordinator
                .server
                .calls
                .iter()
                .filter(|(method, _)| method == "turn/start")
                .count(),
            1
        );
    }

    #[test]
    fn direct_recovery_history_cannot_erase_an_unmediated_tool() {
        let fixture = crate::direct_review_target::tests::Fixture::new(true);
        fixture.commit("candidate\n");
        let reader = fixture.reader(fixture.admit("head", None));
        let mut coordinator = direct_coordinator(reader);
        coordinator
            .start_turn(request("original", DeliveryMode::Submit))
            .unwrap();
        coordinator.abandon().unwrap();
        coordinator.server.replies.push_back(Ok(serde_json::json!({"thread":{"id":"thread-1","parentThreadId":null,"turns":[{"id":"turn-1","status":"completed","items":[{"type":"mcpToolCall","id":"plugin-read"}]}]}})));
        assert!(coordinator.reconcile_history().unwrap());
        assert_eq!(
            coordinator
                .candidate_reader
                .as_ref()
                .unwrap()
                .check_acceptance()
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        assert_eq!(
            coordinator
                .server
                .calls
                .iter()
                .filter(|(method, _)| method == "turn/start")
                .count(),
            1
        );
    }

    #[test]
    fn direct_original_history_response_survives_missing_or_different_notification_output() {
        let fixture = crate::direct_review_target::tests::Fixture::new(true);
        fixture.commit("candidate\n");
        for inline in [
            None,
            Some(
                serde_json::json!([{"type":"agentMessage","text":"different notification response"}]),
            ),
        ] {
            let reader = fixture.reader(fixture.admit("head", None));
            let run = reader.run_id();
            let mut coordinator = direct_coordinator(reader);
            coordinator
                .start_turn(request("original", DeliveryMode::Submit))
                .unwrap();
            coordinator.server.replies.push_back(Ok(serde_json::json!({"thread":{"id":"thread-1","turns":[{"id":"turn-1","status":"completed","items":[{"type":"agentMessage","text":"authoritative original response"}]}]}})));
            let mut turn = serde_json::json!({"id":"turn-1","status":"completed"});
            if let Some(items) = inline {
                turn["items"] = items;
            }
            let terminal = coordinator.ingest(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":turn}})).unwrap().unwrap();
            assert_eq!(
                terminal.final_response,
                Some(FinalResponse::Inline {
                    text: "authoritative original response".to_owned()
                })
            );
            drop(coordinator);
            let reopened =
                crate::direct_review_target::DirectTargetReader::for_run(&fixture.state, run)
                    .unwrap()
                    .unwrap();
            assert_eq!(
                reopened
                    .completed_history_response("thread-1", "turn-1")
                    .unwrap(),
                terminal.final_response
            );
            assert!(
                reopened
                    .completed_history_response("thread-1", "replacement")
                    .is_err()
            );
            reopened.check_acceptance().unwrap();
        }
    }

    #[test]
    fn direct_terminal_history_loss_blocks_acceptance_until_original_startup_readback() {
        let fixture = crate::direct_review_target::tests::Fixture::new(true);
        fixture.commit("candidate\n");
        let reader = fixture.reader(fixture.admit("head", None));
        let run = reader.run_id();
        let mut coordinator = direct_coordinator(reader);
        coordinator
            .start_turn(request("original", DeliveryMode::Submit))
            .unwrap();
        assert!(coordinator.ingest(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[{"type":"agentMessage","text":"clean"}]}}})).is_err());
        assert!(
            coordinator
                .journal()
                .entries
                .iter()
                .any(|entry| matches!(entry, JournalEntry::Terminal { .. }))
        );
        assert!(coordinator.journal().entries.iter().any(|entry|matches!(entry,
            JournalEntry::Terminal{final_response:Some(FinalResponse::Inline{text}),..} if text=="clean")));
        let reopened =
            crate::direct_review_target::DirectTargetReader::for_run(&fixture.state, run)
                .unwrap()
                .unwrap();
        assert_eq!(
            reopened.check_acceptance().unwrap_err().code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        let mut recovered = direct_coordinator(reopened);
        recovered.restore_durable_state(
            RunLifecycle::Idle,
            Some("thread-1".to_owned()),
            None,
            Some("turn-1".to_owned()),
        );
        recovered.server.replies.clear();
        recovered
            .server
            .replies
            .push_back(Ok(serde_json::json!({"cwd":recovered.cwd,"approvalPolicy":"never","sandbox":{"type":"readOnly"},"model":"gpt-5","thread":{"id":"thread-1","parentThreadId":null,"turns":[]}})));
        recovered.server.replies.push_back(Ok(serde_json::json!({"thread":{"id":"thread-1","turns":[{"id":"turn-1","items":[{"type":"commandExecution","id":"hidden-read"}]}]}})));
        recovered
            .complete_connection_startup(RunLifecycle::Idle)
            .unwrap();
        assert!(
            recovered
                .candidate_reader
                .as_ref()
                .unwrap()
                .pending_histories()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            recovered
                .candidate_reader
                .as_ref()
                .unwrap()
                .check_acceptance()
                .unwrap_err()
                .code,
            "REVIEW_CANDIDATE_PROVENANCE_INVALID"
        );
        assert!(
            !recovered
                .server
                .calls
                .iter()
                .any(|(method, _)| method == "turn/start" || method == "thread/start")
        );
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
    fn native_control_conflict_busy_and_completion_keep_original_correlation() {
        let dispatcher = Arc::new(RecordingPrimaryDispatcher::default());
        let session = Uuid::now_v7();
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
                session_id: session,
                contract: PrimaryToolContract::load().unwrap().with_specialists(),
                dispatcher: dispatcher.clone(),
            }),
        );
        coordinator.run_id = session;
        coordinator
            .start_turn(request("native-control", DeliveryMode::Submit))
            .unwrap();
        let frame = |id: u64, call: &str, request: &str| {
            serde_json::json!({"id":id,"method":"item/tool/call","params":{
            "threadId":"thread-1","turnId":"turn-1","callId":call,"tool":crate::native_consumer::MANAGED_CONTROL,
            "arguments":{"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"request","request_id":request}}}})
        };
        let check = |server: &FakeServer, id, request, code| {
            let reply = &server
                .responses
                .iter()
                .find(|(key, _)| *key == id)
                .unwrap()
                .1;
            assert_eq!(reply["success"], false);
            let body: Value =
                serde_json::from_str(reply["contentItems"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(body["ok"], false);
            assert_eq!(body["action"], "status");
            assert_eq!(body["request_id"], request);
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["effect"], "not_called");
            assert!(body.get("data").is_none());
        };
        for index in 0..MAX_PENDING_PRIMARY_TOOLS {
            coordinator
                .ingest(frame(
                    index as u64 + 1,
                    &format!("call-{index}"),
                    &format!("request-{index}"),
                ))
                .unwrap();
        }
        coordinator
            .ingest(frame(1000, "call-0", "changed-request"))
            .unwrap();
        check(
            &coordinator.server,
            1000,
            "changed-request",
            "request_conflict",
        );
        coordinator
            .ingest(frame(1001, "overflow", "busy-request"))
            .unwrap();
        check(&coordinator.server, 1001, "busy-request", "busy");
        assert_eq!(
            dispatcher.calls.lock().unwrap().len(),
            MAX_PENDING_PRIMARY_TOOLS
        );
        coordinator
            .complete_primary_tool(PrimaryToolCompletion {
                run_generation: coordinator.run_generation,
                source_tool_call_id: "call-0".into(),
                request_id: 1,
                result: Err(MachineError::new(
                    "IDEMPOTENCY_CONFLICT",
                    "retained completion conflict",
                    false,
                    serde_json::json!({}),
                )),
            })
            .unwrap();
        check(&coordinator.server, 1, "request-0", "request_conflict");
        assert_eq!(
            coordinator.pending_primary_tools.len(),
            MAX_PENDING_PRIMARY_TOOLS - 1
        );
    }

    #[test]
    fn successor_primary_tool_keeps_native_context_and_separates_legacy_schema() {
        let make = || {
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
                    contract: PrimaryToolContract::load().unwrap().with_specialists(),
                    dispatcher: dispatcher.clone(),
                }),
            );
            coordinator.run_id = session_id;
            coordinator
                .start_turn(request("primary", DeliveryMode::Submit))
                .unwrap();
            let public = serde_json::json!({"schema":"dolgorae.external-specialist-request/v5","operation":"release_specialist","scope_kind":"orchestrated_session","scope_ref":session_id,"member_ref":Uuid::now_v7(),"expected_revision":1});
            let params = serde_json::json!({"threadId":"thread-1","turnId":"turn-1","callId":"actual-call","tool":crate::specialist_facade::MANAGED_TOOL,"arguments":public});
            (coordinator, dispatcher, params)
        };
        let (mut coordinator, dispatcher, params) = make();
        let tools = &coordinator
            .server
            .calls
            .iter()
            .find(|(method, _)| method == "thread/start")
            .unwrap()
            .1["dynamicTools"];
        assert_eq!(tools.as_array().unwrap().len(), 3);
        for tool in tools.as_array().unwrap() {
            assert_eq!(tool["type"], "function");
        }
        assert_eq!(tools[0]["name"], PRIMARY_TOOL_NAME);
        assert_eq!(
            tools[0]["inputSchema"]["oneOf"].as_array().unwrap().len(),
            9
        );
        assert_eq!(tools[1]["name"], crate::native_consumer::MANAGED_NATIVE);
        assert_eq!(tools[2]["name"], crate::native_consumer::MANAGED_CONTROL);
        for tool in &tools.as_array().unwrap()[1..] {
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
        coordinator
            .ingest(serde_json::json!({"id":73,"method":"item/tool/call","params":params}))
            .unwrap();
        let calls = dispatcher.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, crate::specialist_facade::MANAGED_TOOL);
        assert_eq!(calls[0].source_thread_id, "thread-1");
        assert_eq!(calls[0].run_generation, coordinator.run_generation);
        assert_eq!(calls[0].context.session_id, coordinator.run_id);
        assert_eq!(calls[0].context.source_run_id, coordinator.run_id);
        assert_eq!(calls[0].context.source_turn_id, "turn-1");
        assert_eq!(calls[0].context.source_tool_call_id, "actual-call");
        assert_eq!(calls[0].payload, params["arguments"]);
        for (field, value) in [
            ("threadId", "foreign"),
            ("turnId", "foreign"),
            ("callId", ""),
            ("threadId", ""),
            ("tool", "unregistered"),
        ] {
            let (mut coordinator, dispatcher, mut params) = make();
            params[field] = serde_json::json!(value);
            let _ = coordinator
                .ingest(serde_json::json!({"id":72,"method":"item/tool/call","params":params}));
            assert!(
                dispatcher.calls.lock().unwrap().is_empty(),
                "axis {field}={value}"
            );
        }
        let (mut coordinator, dispatcher, mut params) = make();
        params["arguments"]["controller_principal"] = serde_json::json!("model-authority");
        coordinator
            .ingest(serde_json::json!({"id":74,"method":"item/tool/call","params":params}))
            .unwrap();
        assert!(dispatcher.calls.lock().unwrap().is_empty());
        let (id, native) = coordinator.server.responses.last().unwrap();
        assert_eq!(*id, 74);
        assert_eq!(native["success"], false);
        let body: Value =
            serde_json::from_str(native["contentItems"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["schema"], "dolgorae.external-specialist-failure/v5");
        assert_eq!(body["operation"], "release_specialist");
        assert_eq!(body["error"]["effect"], "none");

        let (mut coordinator, dispatcher, params) = make();
        coordinator
            .ingest(serde_json::json!({"id":75,"method":"item/tool/call","params":params}))
            .unwrap();
        let mut changed = params;
        changed["arguments"]["expected_revision"] = serde_json::json!(2);
        coordinator
            .ingest(serde_json::json!({"id":76,"method":"item/tool/call","params":changed}))
            .unwrap();
        assert_eq!(dispatcher.calls.lock().unwrap().len(), 1);
        let (id, native) = coordinator.server.responses.last().unwrap();
        assert_eq!(*id, 76);
        assert_eq!(native["success"], false);
        let body: Value =
            serde_json::from_str(native["contentItems"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["schema"], "dolgorae.external-specialist-failure/v5");
        assert_eq!(body["operation"], "release_specialist");
        assert_eq!(body["error"]["code"], "IDEMPOTENCY_CONFLICT");
        assert_eq!(body["error"]["effect"], "none");
        for invalid_operation in [
            Value::Null,
            serde_json::json!(false),
            serde_json::json!("unknown"),
        ] {
            let (mut coordinator, dispatcher, mut params) = make();
            params["arguments"]["operation"] = invalid_operation;
            coordinator
                .ingest(serde_json::json!({"id":77,"method":"item/tool/call","params":params}))
                .unwrap();
            assert!(dispatcher.calls.lock().unwrap().is_empty());
            let (id, native) = coordinator.server.responses.last().unwrap();
            assert_eq!(*id, 77);
            assert_eq!(native["success"], false);
            let body: Value =
                serde_json::from_str(native["contentItems"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(body["schema"], "dolgorae.external-specialist-failure/v5");
            assert_eq!(body["operation"], "lookup_operation");
            assert_eq!(body["error"]["effect"], "none");
        }
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
            9
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
        assert!(
            coordinator
                .complete_primary_tool_with(
                    PrimaryToolCompletion {
                        request_id: 41,
                        run_generation: 1,
                        source_tool_call_id: "call-1".into(),
                        result: Ok(serde_json::json!({"operation":"list_specialists_result"})),
                    },
                    |_| Err(TurnError::Journal(
                        "injected owning publication failure".into()
                    ))
                )
                .is_err()
        );
        assert_eq!(
            coordinator.pending_primary_tools["call-1"].request_ids,
            vec![41, 42]
        );
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

    fn caller_coordinator() -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        let mut server = FakeServer::default();
        server.replies.push_back(Ok(serde_json::json!({
            "thread": {"id":"caller-thread", "turns":[]}, "model":"gpt-5",
            "cwd":"/tmp/workspace", "approvalPolicy":"never",
            "sandbox":turn_sandbox("read-only", Path::new("/tmp/workspace")),
        })));
        server.replies.push_back(Ok(serde_json::json!({})));
        server.replies.push_back(Ok(serde_json::json!({
            "thread":{"id":"caller-thread", "turns":[]},
        })));
        let mut coordinator = coordinator_in(
            server,
            ThreadAttach::Start,
            Box::new(RecordingDiagnostics::default()),
            SessionSafetyPolicy::CallerWorkspace,
        );
        coordinator.approval_policy = "never".to_owned();
        coordinator
    }

    pub(crate) fn assert_startup_process_context(runtime: crate::worker::QualifiedCallerRuntime) {
        let mut coordinator = caller_coordinator();
        let initial_calls = coordinator.server.calls.len();
        let initial_journal = coordinator.journal.entries.clone();
        coordinator.caller_runtime = Some(runtime);
        coordinator.server.replies.clear();
        for context in ["retained_thread_preparation", "original_thread_binding"] {
            coordinator.caller_startup_phase = Some(context);
            coordinator.caller_startup_failure = None;
            coordinator.server.replies.push_back(Ok(serde_json::json!({
                "config":{"project_doc_max_bytes":0},
                "layers":[{"name":{"type":"sessionFlags"},"config":{"project_doc_max_bytes":0}}],
            })));
            let error = coordinator.check_caller_process_quiescence().unwrap_err();
            coordinator.record_caller_startup_failure(&error);
            let diagnostic = coordinator.caller_startup_failure().unwrap();
            assert_eq!(diagnostic["caller_context"], context);
            assert_eq!(diagnostic["substage"], "process_quiescence");
            assert_eq!(
                diagnostic["process_proof"]["predicate"],
                "startup_quiescence_timeout"
            );
            assert_eq!(
                diagnostic["process_proof"]["observation"]["proof_entry_remaining_ms"],
                0
            );
            assert_eq!(error, TurnError::TurnBusy);
        }
        assert_eq!(coordinator.server.calls.len(), initial_calls + 2);
        assert!(
            coordinator
                .server
                .calls
                .iter()
                .skip(initial_calls)
                .all(|(method, _)| method == "config/read")
        );
        assert_eq!(coordinator.journal.entries, initial_journal);
    }

    fn settings_notification(write: bool) -> Value {
        serde_json::json!({"method":"thread/settings/updated", "params":{
            "threadId":"caller-thread", "threadSettings":{
                "cwd":"/tmp/workspace", "approvalPolicy":if write {"on-request"} else {"never"},
                "sandboxPolicy":turn_sandbox(if write {"workspace-write"} else {"read-only"}, Path::new("/tmp/workspace")),
            },
        }})
    }

    #[test]
    fn caller_policy_verification_retains_the_refusal_without_retrying_or_starting_a_turn() {
        for (case, phase, kind) in [
            ("loaded_root", "loaded_root_before_status", "TurnBusy"),
            ("read_transport", "thread_status_readback", "Transport"),
            ("proof", "policy_proof", "CorrelationMismatch"),
        ] {
            let mut coordinator = caller_coordinator();
            coordinator.initialize_thread(Uuid::now_v7()).unwrap();
            let journal_len = coordinator.journal.entries.len();
            let calls = coordinator.server.calls.len();
            let loaded = serde_json::json!({"data":["caller-thread"],"nextCursor":null});
            match case {
                "loaded_root" => coordinator.server.replies.push_back(Ok(
                    serde_json::json!({"data":["untrusted-provider-content"],"nextCursor":null}),
                )),
                "read_transport" => {
                    coordinator.server.replies.push_back(Ok(loaded));
                    coordinator
                        .server
                        .replies
                        .push_back(Err(TurnError::transport(
                            TransportStage::Read,
                            "untrusted-provider-content",
                        )));
                }
                "proof" => {
                    coordinator.server.replies.push_back(Ok(loaded.clone()));
                    coordinator.server.replies.push_back(Ok(serde_json::json!({
                        "thread":{"id":"caller-thread","parentThreadId":null,
                            "status":{"type":"idle"}},
                    })));
                    coordinator.server.replies.push_back(Ok(loaded));
                }
                _ => unreachable!(),
            }
            let error = coordinator
                .verify_caller_policy(Uuid::now_v7())
                .unwrap_err();
            assert_eq!(error.diagnostic_kind(), kind, "{case}");
            let diagnostic = coordinator.caller_startup_failure().unwrap();
            assert_eq!(diagnostic["substage"], phase, "{case}");
            assert_eq!(diagnostic["original_error"]["kind"], kind, "{case}");
            assert!(
                !diagnostic
                    .to_string()
                    .contains("untrusted-provider-content")
            );
            if case == "read_transport" {
                assert_eq!(diagnostic["original_error"]["transport_stage"], "read");
            }
            assert!(coordinator.caller_startup_phase.is_none());
            assert_eq!(coordinator.state(), &CoordinatorState::Idle);
            assert_eq!(coordinator.thread_id(), Some("caller-thread"));
            assert_eq!(coordinator.journal.entries.len(), journal_len);
            assert_eq!(
                coordinator.server.calls.len() - calls,
                if case == "loaded_root" {
                    1
                } else if case == "read_transport" {
                    2
                } else {
                    3
                }
            );
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
        }
    }

    #[test]
    fn caller_connection_enables_settings_api_without_adding_dynamic_tools() {
        let mut caller = caller_coordinator();
        assert!(caller.server.experimental_api);
        caller.initialize_thread(Uuid::now_v7()).unwrap();
        let (_, params) = caller
            .server
            .calls
            .iter()
            .find(|(method, _)| method == "thread/start")
            .unwrap();
        assert!(params.get("dynamicTools").is_none());
        for safety in [
            SessionSafetyPolicy::Standard,
            SessionSafetyPolicy::ReviewerReadOnly,
        ] {
            let other = coordinator_in(
                FakeServer::default(),
                ThreadAttach::Start,
                Box::new(RecordingDiagnostics::default()),
                safety,
            );
            assert!(!other.server.experimental_api);
        }
    }

    #[test]
    fn caller_provisional_thread_recovery_resumes_original_as_reader_without_task_replay() {
        let mut caller = caller_coordinator();
        caller.server.replies.clear();
        caller.server.replies.push_back(Ok(serde_json::json!({
            "thread":{"id":"caller-thread","turns":[]}, "model":"gpt-5",
            "cwd":"/tmp/workspace", "approvalPolicy":"never",
            "sandbox":turn_sandbox("read-only", Path::new("/tmp/workspace")),
        })));
        caller.restore_durable_state(RunLifecycle::OutcomeUnknown, None, None, None);
        caller
            .restore_turn_receipts(vec![(
                AuditKind::TurnIntent,
                serde_json::json!({
                    "operation_id":Uuid::now_v7(), "provisional_thread_id":"caller-thread",
                }),
            )])
            .unwrap();
        caller.sandbox = "workspace-write".to_owned();
        caller.writer_generation = Some(7);
        let before_resume = caller.server.calls.len();
        caller
            .complete_connection_startup(RunLifecycle::OutcomeUnknown)
            .unwrap();
        assert_eq!(caller.state(), &CoordinatorState::OutcomeUnknown);
        assert_eq!(caller.thread_id(), Some("caller-thread"));
        assert!(caller.writer_generation.is_none());
        assert_eq!(caller.server.calls.len(), before_resume + 1);
        let (method, params) = caller.server.calls.last().unwrap();
        assert_eq!(method, "thread/resume");
        assert_eq!(params["threadId"], "caller-thread");
        assert_eq!(params["sandbox"], "read-only");
        assert_eq!(params["approvalPolicy"], "never");
        prepend_caller_quiescence(&mut caller.server);
        caller.server.replies.push_back(Ok(serde_json::json!({
            "thread":{"id":"caller-thread","turns":[]},
        })));
        assert!(caller.reconcile_history().unwrap());
        assert_eq!(caller.state(), &CoordinatorState::Paused);
        assert!(!caller.server.calls.iter().any(|(method, _)| matches!(
            method.as_str(),
            "thread/start" | "turn/start" | "thread/settings/update"
        )));
    }

    #[test]
    fn caller_unknown_startup_without_durable_thread_never_creates_replacement() {
        let mut caller = caller_coordinator();
        caller.restore_durable_state(RunLifecycle::OutcomeUnknown, None, None, None);
        let calls = caller.server.calls.len();
        assert_eq!(
            caller.complete_connection_startup(RunLifecycle::OutcomeUnknown),
            Err(TurnError::OutcomeUnknown)
        );
        assert_eq!(caller.server.calls.len(), calls);
        assert!(
            caller
                .restore_turn_receipts(vec![
                    (
                        AuditKind::TurnIntent,
                        serde_json::json!({"provisional_thread_id":"original"})
                    ),
                    (
                        AuditKind::TurnIntent,
                        serde_json::json!({"provisional_thread_id":"replacement"})
                    ),
                ])
                .is_err()
        );
    }

    #[test]
    fn caller_incomplete_startup_prefix_is_quarantined_even_without_recorded_unknown() {
        for provisional in [false, true] {
            let mut caller = caller_coordinator();
            caller.server.replies.clear();
            caller.restore_durable_state(RunLifecycle::Idle, None, None, None);
            let mut records = vec![(
                AuditKind::TurnIntent,
                serde_json::json!({
                    "schema":"dolgorae.caller-native-intent/v1", "operation_id":Uuid::now_v7(), "method":"thread/start",
                }),
            )];
            if provisional {
                records.push((
                    AuditKind::TurnIntent,
                    serde_json::json!({"provisional_thread_id":"caller-thread"}),
                ));
                caller.server.replies.push_back(Ok(serde_json::json!({
                    "thread":{"id":"caller-thread","turns":[]},"model":"gpt-5","cwd":"/tmp/workspace",
                    "approvalPolicy":"never", "sandbox":turn_sandbox("read-only",Path::new("/tmp/workspace")),
                })));
            }
            caller.restore_turn_receipts(records).unwrap();
            assert_eq!(caller.state(), &CoordinatorState::OutcomeUnknown);
            let connected = caller.complete_connection_startup(RunLifecycle::Idle);
            assert_eq!(connected.is_ok(), provisional);
            assert_eq!(caller.state(), &CoordinatorState::OutcomeUnknown);
            assert_eq!(caller.thread_id(), provisional.then_some("caller-thread"));
            assert!(
                !caller
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| matches!(method.as_str(), "thread/start" | "turn/start"))
            );
        }
    }

    #[test]
    fn candidate_reviewer_crash_prefix_never_creates_a_replacement_thread_or_turn() {
        for lifecycle in [RunLifecycle::Idle, RunLifecycle::OutcomeUnknown] {
            for provisional in [false, true] {
                let mut reviewer = caller_coordinator();
                reviewer.safety_policy = SessionSafetyPolicy::ReviewerReadOnly;
                reviewer.candidate_tools = true;
                reviewer.server.replies.clear();
                reviewer.restore_durable_state(lifecycle, None, None, None);
                let mut records = vec![(
                    AuditKind::TurnIntent,
                    serde_json::json!({
                        "schema":"dolgorae.caller-native-intent/v1", "operation_id":Uuid::now_v7(),
                        "method":"thread/start",
                    }),
                )];
                if provisional {
                    records.push((
                        AuditKind::TurnIntent,
                        serde_json::json!({
                            "provisional_thread_id":"caller-thread",
                        }),
                    ));
                    reviewer.server.replies.push_back(Ok(serde_json::json!({
                        "thread":{"id":"caller-thread","turns":[],"parentThreadId":null},
                        "model":"gpt-5","cwd":"/tmp/workspace","approvalPolicy":"never",
                        "sandbox":turn_sandbox("read-only",Path::new("/tmp/workspace")),
                    })));
                }
                reviewer.restore_turn_receipts(records).unwrap();
                assert_eq!(reviewer.state(), &CoordinatorState::OutcomeUnknown);
                reviewer.sandbox = "workspace-write".to_owned();
                reviewer.writer_generation = Some(7);
                let connected = reviewer.complete_connection_startup(lifecycle);
                assert_eq!(connected.is_ok(), provisional);
                assert_eq!(reviewer.state(), &CoordinatorState::OutcomeUnknown);
                assert_eq!(reviewer.thread_id(), provisional.then_some("caller-thread"));
                if provisional {
                    assert!(reviewer.writer_generation.is_none());
                    let (method, params) = reviewer.server.calls.last().unwrap();
                    assert_eq!(method, "thread/resume");
                    assert_eq!(params["threadId"], "caller-thread");
                    assert_eq!(params["sandbox"], "read-only");
                    assert_eq!(params["approvalPolicy"], "never");
                    prepend_caller_quiescence(&mut reviewer.server);
                    reviewer.server.replies.push_back(Ok(serde_json::json!({
                        "thread":{"id":"caller-thread","turns":[]},
                    })));
                    assert!(reviewer.reconcile_history().unwrap());
                    assert_eq!(reviewer.state(), &CoordinatorState::Paused);
                }
                assert!(!reviewer.server.calls.iter().any(|(method, _)| matches!(
                    method.as_str(),
                    "thread/start" | "turn/start" | "thread/settings/update"
                )));
            }
        }
    }

    #[test]
    fn caller_discovery_requires_merged_zero_and_enabled_process_override() {
        let valid = serde_json::json!({"config":{"project_doc_max_bytes":0},"layers":[{
            "name":{"type":"sessionFlags"}, "config":{"project_doc_max_bytes":0}, "disabledReason":null,
        }]});
        for variant in [
            "valid",
            "managed_override",
            "project_only",
            "disabled",
            "missing",
            "float",
        ] {
            let mut reply = valid.clone();
            match variant {
                "managed_override" => {
                    reply["config"]["project_doc_max_bytes"] = serde_json::json!(32768)
                }
                "project_only" => reply["layers"][0]["name"]["type"] = serde_json::json!("project"),
                "disabled" => {
                    reply["layers"][0]["disabledReason"] = serde_json::json!("managed policy")
                }
                "missing" => {
                    reply["config"]
                        .as_object_mut()
                        .unwrap()
                        .remove("project_doc_max_bytes");
                }
                "float" => reply["config"]["project_doc_max_bytes"] = serde_json::json!(0.0),
                _ => {}
            }
            let mut caller = caller_coordinator();
            caller.server.replies.clear();
            caller.server.replies.push_back(Ok(reply));
            assert_eq!(
                caller.check_caller_configuration().is_ok(),
                variant == "valid",
                "{variant}"
            );
            assert!(
                !caller
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| matches!(method.as_str(), "thread/start" | "turn/start"))
            );
        }
    }

    fn prepend_caller_quiescence(server: &mut FakeServer) {
        for reply in [
            serde_json::json!({"data":["caller-thread"], "nextCursor":null}),
            serde_json::json!({"thread":{"id":"caller-thread", "parentThreadId":null,
                "status":{"type":"idle"}}}),
            serde_json::json!({"data":["caller-thread"], "nextCursor":null}),
        ]
        .into_iter()
        .rev()
        {
            server.replies.push_front(Ok(reply));
        }
    }

    #[test]
    fn caller_binding_and_settings_journal_failures_quarantine_without_recorded_unknown() {
        struct FailAfterThreadBound {
            fail_binding: bool,
        }
        impl DurableTurnJournal for FailAfterThreadBound {
            fn append_and_sync(&mut self, entry: JournalEntry) -> Result<(), TurnError> {
                if matches!(entry, JournalEntry::ProvisionalThread { .. })
                    || (!self.fail_binding && matches!(entry, JournalEntry::ThreadBound { .. }))
                {
                    Ok(())
                } else {
                    Err(TurnError::Journal(
                        "injected durable-write failure".to_owned(),
                    ))
                }
            }
        }
        for fail_binding in [true, false] {
            let mut server = FakeServer::default();
            server.replies.push_back(Ok(serde_json::json!({
                "thread":{"id":"caller-thread", "turns":[]}, "model":"gpt-5",
                "cwd":"/tmp/workspace", "approvalPolicy":"never",
                "sandbox":turn_sandbox("read-only", Path::new("/tmp/workspace")),
            })));
            server.replies.push_back(Ok(serde_json::json!({})));
            server.replies.push_back(Ok(serde_json::json!({
                "thread":{"id":"caller-thread", "turns":[]},
            })));
            let mut coordinator = coordinator_with_journal(
                server,
                ThreadAttach::Start,
                Box::new(RecordingDiagnostics::default()),
                SessionSafetyPolicy::CallerWorkspace,
                None,
                FailAfterThreadBound { fail_binding },
            );
            coordinator.approval_policy = "never".to_owned();
            let initialized = coordinator.initialize_thread(Uuid::now_v7());
            if fail_binding {
                assert!(initialized.is_err());
                assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
                assert_eq!(coordinator.thread_id(), Some("caller-thread"));
                let writes = coordinator.server.calls.len();
                assert!(coordinator.initialize_thread(Uuid::now_v7()).is_err());
                assert_eq!(coordinator.server.calls.len(), writes);
                continue;
            }
            initialized.unwrap();
            prepend_caller_quiescence(&mut coordinator.server);
            coordinator
                .server
                .replies
                .push_back(Ok(serde_json::json!({})));
            coordinator
                .server
                .messages
                .push_back(settings_notification(true));
            assert!(
                coordinator
                    .set_writer_access(true, 7, Uuid::now_v7())
                    .is_err()
            );
            assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
            let writes = coordinator.server.calls.len();
            assert!(
                coordinator
                    .set_writer_access(false, 7, Uuid::now_v7())
                    .is_err()
            );
            assert_eq!(coordinator.server.calls.len(), writes);
        }
    }

    #[test]
    fn caller_thread_and_native_settings_updates_need_no_task_turn() {
        let mut coordinator = caller_coordinator();
        coordinator.initialize_thread(Uuid::now_v7()).unwrap();
        assert_eq!(coordinator.state(), &CoordinatorState::Idle);
        for (write, generation) in [(true, 7), (false, 7), (true, 8)] {
            prepend_caller_quiescence(&mut coordinator.server);
            coordinator
                .server
                .replies
                .push_back(Ok(serde_json::json!({})));
            coordinator
                .server
                .messages
                .push_back(settings_notification(write));
            coordinator
                .set_writer_access(write, generation, Uuid::now_v7())
                .unwrap();
            assert_eq!(coordinator.thread_id(), Some("caller-thread"));
            let Some(JournalEntry::WriterAccessChanged {
                effective_policy, ..
            }) = coordinator.journal.entries.last()
            else {
                panic!("missing durable policy observation");
            };
            // Metadata alone cannot promote empirical enforcement qualification.
            assert_eq!(effective_policy.access, crate::domain::Access::Unknown);
            assert_eq!(
                effective_policy.verification,
                crate::domain::PolicyVerification::Unverified
            );
            assert_eq!(
                effective_policy.writer_generation,
                write.then_some(generation)
            );
        }
        let calls = &coordinator.server.calls;
        assert_eq!(
            calls
                .iter()
                .filter(|(method, _)| method == "thread/start")
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(method, _)| method == "thread/settings/update")
                .count(),
            3
        );
        assert!(!calls.iter().any(|(method, _)| method == "turn/start"));
        assert_eq!(
            calls
                .iter()
                .map(|(method, _)| method.as_str())
                .take(5)
                .collect::<Vec<_>>(),
            [
                "initialize",
                "initialized",
                "thread/start",
                "thread/name/set",
                "thread/read"
            ]
        );
        let name = &calls
            .iter()
            .find(|(method, _)| method == "thread/name/set")
            .unwrap()
            .1;
        assert_eq!(name["threadId"], "caller-thread");
        assert_eq!(name["name"], format!("Dolgorae Run {}", coordinator.run_id));
        let read = &calls
            .iter()
            .find(|(method, _)| method == "thread/read")
            .unwrap()
            .1;
        assert_eq!(
            read,
            &serde_json::json!({"threadId":"caller-thread", "includeTurns":true})
        );
        let params = &calls
            .iter()
            .find(|(method, _)| method == "thread/start")
            .unwrap()
            .1;
        assert_eq!(params["config"]["project_doc_max_bytes"], 0);
        assert_eq!(
            params["config"]["sandbox_workspace_write"]["network_access"],
            false
        );
    }

    #[test]
    fn caller_persistence_barrier_failure_retains_original_thread_and_fences_retry() {
        for failure in [
            "name_transport",
            "name_refused",
            "name_shape",
            "read_transport",
            "read_refused",
            "read_identity",
            "read_history",
            "read_missing_history",
        ] {
            let mut coordinator = caller_coordinator();
            match failure {
                "name_transport" => {
                    coordinator.server.replies[1] = Err(TurnError::transport(
                        TransportStage::Read,
                        "lost metadata response",
                    ))
                }
                "name_refused" => {
                    coordinator.server.replies[1] = Err(TurnError::AppServerRejected {
                        code: -32601,
                        message: "unsupported".to_owned(),
                    })
                }
                "name_shape" => coordinator.server.replies[1] = Ok(Value::Null),
                "read_transport" => {
                    coordinator.server.replies[2] =
                        Err(TurnError::transport(TransportStage::Read, "lost readback"))
                }
                "read_refused" => {
                    coordinator.server.replies[2] = Err(TurnError::AppServerRejected {
                        code: -32600,
                        message: "not persisted".to_owned(),
                    })
                }
                "read_identity" => {
                    coordinator.server.replies[2] = Ok(serde_json::json!({
                        "thread":{"id":"replacement-thread", "turns":[]},
                    }))
                }
                "read_history" => {
                    coordinator.server.replies[2] = Ok(serde_json::json!({
                        "thread":{"id":"caller-thread", "turns":[{"id":"unexpected-turn"}]},
                    }))
                }
                "read_missing_history" => {
                    coordinator.server.replies[2] = Ok(serde_json::json!({
                        "thread":{"id":"caller-thread"},
                    }))
                }
                _ => unreachable!(),
            }
            let error = coordinator.initialize_thread(Uuid::now_v7()).unwrap_err();
            let diagnostic = coordinator.caller_startup_failure().unwrap().clone();
            assert_eq!(
                diagnostic["substage"],
                if failure.starts_with("name_") {
                    "thread/name/set"
                } else {
                    "thread_persistence_readback"
                },
                "{failure}"
            );
            let expected_kind = if failure.ends_with("transport") {
                "Transport"
            } else if failure.ends_with("refused") {
                "AppServerRejected"
            } else {
                "CorrelationMismatch"
            };
            assert_eq!(
                diagnostic["original_error"]["kind"], expected_kind,
                "{failure}"
            );
            if expected_kind == "Transport" {
                assert_eq!(diagnostic["original_error"]["transport_stage"], "read");
            }
            for prose in [
                "lost metadata response",
                "unsupported",
                "lost readback",
                "not persisted",
            ] {
                assert!(!diagnostic.to_string().contains(prose), "{failure}");
            }
            assert!(coordinator.caller_startup_phase.is_none());
            let machine = error.into_machine_error(&TurnFailureContext {
                run_id: coordinator.run_id,
                profile: "default".to_owned(),
                thread_id: coordinator.thread_id().map(str::to_owned),
                turn_id: None,
            });
            assert_eq!(machine.code, "OUTCOME_UNKNOWN", "{failure}");
            assert!(!machine.retryable, "{failure}");
            assert_eq!(
                coordinator.state(),
                &CoordinatorState::OutcomeUnknown,
                "{failure}"
            );
            assert_eq!(coordinator.thread_id(), Some("caller-thread"), "{failure}");
            assert!(
                coordinator
                    .journal
                    .entries
                    .iter()
                    .any(|entry| matches!(entry,
                JournalEntry::ProvisionalThread { thread_id, .. } if thread_id == "caller-thread"))
            );
            assert!(
                !coordinator
                    .journal
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, JournalEntry::ThreadBound { .. }))
            );
            let writes = coordinator.server.calls.len();
            assert!(coordinator.initialize_thread(Uuid::now_v7()).is_err());
            assert_eq!(coordinator.server.calls.len(), writes, "{failure}");
            assert_eq!(coordinator.caller_startup_failure(), Some(&diagnostic));
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
        }
    }

    #[test]
    fn candidate_reviewer_startup_refusals_retain_diagnostics_and_fence_retry() {
        for failure in [
            "settings",
            "name_transport",
            "name_refused",
            "read_transport",
            "read_refused",
        ] {
            let mut coordinator = caller_coordinator();
            coordinator.safety_policy = SessionSafetyPolicy::ReviewerReadOnly;
            coordinator.candidate_tools = true;
            coordinator.server.replies[0].as_mut().unwrap()["thread"]["parentThreadId"] =
                Value::Null;
            match failure {
                "settings" => {
                    coordinator.server.replies[0].as_mut().unwrap()["cwd"] =
                        serde_json::json!("/other-workspace");
                }
                "name_transport" | "read_transport" => {
                    let index = if failure.starts_with("name_") { 1 } else { 2 };
                    coordinator.server.replies[index] = Err(TurnError::transport(
                        TransportStage::Read,
                        "untrusted native refusal prose",
                    ));
                }
                "name_refused" | "read_refused" => {
                    let index = if failure.starts_with("name_") { 1 } else { 2 };
                    coordinator.server.replies[index] = Err(TurnError::AppServerRejected {
                        code: -32600,
                        message: "untrusted native refusal prose".to_owned(),
                    });
                }
                _ => unreachable!(),
            }
            let error = coordinator.initialize_thread(Uuid::now_v7()).unwrap_err();
            let diagnostic = coordinator.caller_startup_failure().unwrap().clone();
            assert_eq!(
                diagnostic["substage"],
                if failure == "settings" {
                    "thread_settings"
                } else if failure.starts_with("name_") {
                    "thread/name/set"
                } else {
                    "thread_persistence_readback"
                },
                "{failure}"
            );
            assert_eq!(
                diagnostic["original_error"]["kind"],
                if failure == "settings" {
                    "CorrelationMismatch"
                } else if failure.ends_with("transport") {
                    "Transport"
                } else {
                    "AppServerRejected"
                },
                "{failure}"
            );
            if failure.ends_with("transport") {
                assert_eq!(diagnostic["original_error"]["transport_stage"], "read");
            } else if failure.ends_with("refused") {
                assert_eq!(diagnostic["original_error"]["rpc_code"], -32600);
            }
            assert!(
                !diagnostic
                    .to_string()
                    .contains("untrusted native refusal prose")
            );
            assert_eq!(
                coordinator.safety_policy,
                SessionSafetyPolicy::ReviewerReadOnly
            );
            assert_eq!(coordinator.thread_id(), Some("caller-thread"));
            assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
            assert!(coordinator.caller_startup_phase.is_none());
            assert!(
                coordinator
                    .journal
                    .entries
                    .iter()
                    .any(|entry| matches!(entry,
                JournalEntry::ProvisionalThread { thread_id, .. } if thread_id == "caller-thread"))
            );
            assert!(!coordinator.journal.entries.iter().any(|entry| matches!(
                entry,
                JournalEntry::ThreadBound { .. } | JournalEntry::TurnAccepted { .. }
            )));
            let machine = error.into_machine_error(&TurnFailureContext {
                run_id: coordinator.run_id,
                profile: "default".to_owned(),
                thread_id: coordinator.thread_id().map(str::to_owned),
                turn_id: None,
            });
            assert_eq!(machine.code, "OUTCOME_UNKNOWN", "{failure}");
            assert!(!machine.retryable, "{failure}");
            let calls = coordinator.server.calls.len();
            assert_eq!(
                coordinator.initialize_thread(Uuid::now_v7()),
                Err(TurnError::OutcomeUnknown)
            );
            assert_eq!(coordinator.server.calls.len(), calls);
            assert_eq!(coordinator.caller_startup_failure(), Some(&diagnostic));
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
        }
    }

    #[test]
    fn candidate_reviewer_retained_startup_keeps_the_original_transport_failure() {
        let mut coordinator = caller_coordinator();
        coordinator.safety_policy = SessionSafetyPolicy::ReviewerReadOnly;
        coordinator.candidate_tools = true;
        coordinator.server.replies[0].as_mut().unwrap()["thread"]["parentThreadId"] = Value::Null;
        coordinator.initialize_thread(Uuid::now_v7()).unwrap();
        coordinator
            .server
            .replies
            .push_back(Err(TurnError::transport(
                TransportStage::Read,
                "untrusted retained refusal prose",
            )));
        assert_eq!(
            coordinator.rebind_retained_thread(),
            Err(TurnError::OutcomeUnknown)
        );
        let diagnostic = coordinator.caller_startup_failure().unwrap().clone();
        assert_eq!(diagnostic["substage"], "thread/resume");
        assert_eq!(diagnostic["original_error"]["kind"], "Transport");
        assert_eq!(diagnostic["original_error"]["transport_stage"], "read");
        assert!(
            !diagnostic
                .to_string()
                .contains("untrusted retained refusal prose")
        );
        assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
        assert!(
            !coordinator
                .server
                .calls
                .iter()
                .any(|(method, _)| method == "turn/start")
        );
    }

    #[test]
    fn caller_startup_status_refusals_keep_the_original_substage_and_retry_fence() {
        for (failure, phase) in [
            (0, "loaded_root_before_status"),
            (1, "thread_status_readback"),
            (2, "loaded_root_after_status"),
        ] {
            let mut coordinator = caller_coordinator();
            coordinator.caller_startup_phase = Some("thread_preparation");
            let operation_id = Uuid::now_v7();
            let original = coordinator.ensure_thread(operation_id).unwrap();
            prepend_caller_quiescence(&mut coordinator.server);
            if failure == 1 {
                coordinator.server.replies[1] = Ok(serde_json::json!({
                    "thread":{"id":"caller-thread", "parentThreadId":null,
                        "status":{"type":"active"}},
                }));
            } else {
                coordinator.server.replies[failure] = Ok(serde_json::json!({
                    "data":["caller-thread", "native-child"], "nextCursor":null,
                }));
            }
            let error = coordinator
                .check_caller_transition_quiescence(&original)
                .unwrap_err();
            coordinator.record_caller_startup_failure(&error);
            coordinator.caller_startup_phase = None;
            assert_eq!(
                coordinator.lost_after_write(operation_id, error),
                TurnError::OutcomeUnknown
            );
            let diagnostic = coordinator.caller_startup_failure().unwrap();
            assert_eq!(diagnostic["substage"], phase);
            assert_eq!(diagnostic["original_error"]["kind"], "TurnBusy");
            assert_eq!(coordinator.thread_id(), Some(original.as_str()));
            assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
            assert!(
                !coordinator
                    .journal
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, JournalEntry::ThreadBound { .. }))
            );
            let calls = coordinator.server.calls.len();
            assert_eq!(
                coordinator.initialize_thread(Uuid::now_v7()),
                Err(TurnError::OutcomeUnknown)
            );
            assert_eq!(coordinator.server.calls.len(), calls);
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
        }
    }

    #[test]
    fn caller_native_child_or_unknown_census_blocks_policy_mutation_without_losing_state() {
        for failure in [
            "child",
            "late_child",
            "empty",
            "paged",
            "malformed",
            "read_identity",
            "read_parent",
            "read_running",
            "read_unknown",
            "transport",
            "unsupported",
        ] {
            let mut coordinator = caller_coordinator();
            coordinator.initialize_thread(Uuid::now_v7()).unwrap();
            prepend_caller_quiescence(&mut coordinator.server);
            match failure {
                "child" => {
                    coordinator.server.replies[0] = Ok(serde_json::json!({
                        "data":["caller-thread", "idle-child"], "nextCursor":null,
                    }))
                }
                "late_child" => {
                    coordinator.server.replies[2] = Ok(serde_json::json!({
                        "data":["caller-thread", "late-child"], "nextCursor":null,
                    }))
                }
                "empty" => {
                    coordinator.server.replies[0] = Ok(serde_json::json!({
                        "data":[], "nextCursor":null,
                    }))
                }
                "paged" => {
                    coordinator.server.replies[0] = Ok(serde_json::json!({
                        "data":["caller-thread"], "nextCursor":"hidden-child",
                    }))
                }
                "malformed" => coordinator.server.replies[0] = Ok(Value::Null),
                "transport" => {
                    coordinator.server.replies[0] = Err(TurnError::transport(
                        TransportStage::Read,
                        "census unavailable",
                    ))
                }
                "unsupported" => {
                    coordinator.server.replies[0] = Err(TurnError::AppServerRejected {
                        code: -32601,
                        message: "unsupported census".to_owned(),
                    })
                }
                field => {
                    let mut read = serde_json::json!({"thread":{"id":"caller-thread",
                        "parentThreadId":null, "status":{"type":"idle"}}});
                    match field {
                        "read_identity" => {
                            read["thread"]["id"] = serde_json::json!("foreign-thread")
                        }
                        "read_parent" => {
                            read["thread"]["parentThreadId"] = serde_json::json!("foreign-parent")
                        }
                        "read_running" => {
                            read["thread"]["status"]["type"] = serde_json::json!("active")
                        }
                        "read_unknown" => {
                            read["thread"].as_object_mut().unwrap().remove("status");
                        }
                        _ => unreachable!(),
                    }
                    coordinator.server.replies[1] = Ok(read);
                }
            }
            let journal = coordinator.journal.entries.clone();
            assert!(
                coordinator
                    .set_writer_access(true, 7, Uuid::now_v7())
                    .is_err(),
                "{failure}"
            );
            assert_eq!(coordinator.state(), &CoordinatorState::Idle, "{failure}");
            assert_eq!(coordinator.thread_id(), Some("caller-thread"));
            assert_eq!(coordinator.sandbox, "read-only");
            assert!(coordinator.writer_generation.is_none());
            assert_eq!(coordinator.journal.entries, journal);
            assert!(!coordinator.server.calls.iter().any(|(method, _)|
                method == "thread/settings/update" || method == "turn/start"));
        }
    }

    #[test]
    fn caller_post_write_busy_is_unknown_but_initial_native_refusal_is_retryable() {
        let mut coordinator = caller_coordinator();
        coordinator.server.replies[0] = Err(TurnError::AppServerRejected {
            code: -32600,
            message: "start declined".to_owned(),
        });
        let error = coordinator.initialize_thread(Uuid::now_v7()).unwrap_err();
        let context = TurnFailureContext {
            run_id: coordinator.run_id,
            profile: "default".to_owned(),
            thread_id: None,
            turn_id: None,
        };
        assert!(error.into_machine_error(&context).retryable);
        assert_eq!(coordinator.state(), &CoordinatorState::Threadless);
        assert!(coordinator.thread_id().is_none());
        assert!(coordinator.journal.entries.is_empty());

        let mut coordinator = caller_coordinator();
        coordinator.initialize_thread(Uuid::now_v7()).unwrap();
        let error = coordinator.lost_after_write(Uuid::now_v7(), TurnError::TurnBusy);
        let context = TurnFailureContext {
            run_id: coordinator.run_id,
            profile: "default".to_owned(),
            thread_id: Some("caller-thread".to_owned()),
            turn_id: None,
        };
        let machine = error.into_machine_error(&context);
        assert_eq!(machine.code, "OUTCOME_UNKNOWN");
        assert!(!machine.retryable);
        assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
        assert_eq!(coordinator.thread_id(), Some("caller-thread"));
    }

    #[test]
    fn caller_settings_ambiguity_retains_unknown_outcome_without_local_policy_change() {
        for failure in [
            "missing", "foreign", "cwd", "approval", "sandbox", "roots", "network",
        ] {
            let mut coordinator = caller_coordinator();
            coordinator.initialize_thread(Uuid::now_v7()).unwrap();
            prepend_caller_quiescence(&mut coordinator.server);
            coordinator
                .server
                .replies
                .push_back(Ok(serde_json::json!({})));
            if failure != "missing" {
                let mut message = settings_notification(true);
                let params = message.get_mut("params").unwrap();
                match failure {
                    "foreign" => params["threadId"] = serde_json::json!("another-thread"),
                    "cwd" => params["threadSettings"]["cwd"] = serde_json::json!("/tmp/another"),
                    "approval" => {
                        params["threadSettings"]["approvalPolicy"] = serde_json::json!("never")
                    }
                    "sandbox" => {
                        params["threadSettings"]["sandboxPolicy"]["type"] =
                            serde_json::json!("dangerFullAccess")
                    }
                    "roots" => {
                        params["threadSettings"]["sandboxPolicy"]["writableRoots"] =
                            serde_json::json!(["/"])
                    }
                    "network" => {
                        params["threadSettings"]["sandboxPolicy"]["networkAccess"] =
                            serde_json::json!(true)
                    }
                    _ => unreachable!(),
                }
                coordinator.server.messages.push_back(message);
            }
            assert!(
                coordinator
                    .set_writer_access(true, 7, Uuid::now_v7())
                    .is_err(),
                "{failure}"
            );
            assert_eq!(
                coordinator.state(),
                &CoordinatorState::OutcomeUnknown,
                "{failure}"
            );
            assert_eq!(coordinator.sandbox, "read-only");
            assert!(coordinator.writer_generation.is_none());
            assert!(
                !coordinator
                    .journal
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, JournalEntry::WriterAccessChanged { .. }))
            );
        }
    }

    #[test]
    fn caller_startup_rejects_existing_turns_or_widened_policy() {
        for mismatch in ["history", "model", "cwd", "network"] {
            let mut coordinator = caller_coordinator();
            let reply = coordinator
                .server
                .replies
                .front_mut()
                .unwrap()
                .as_mut()
                .unwrap();
            match mismatch {
                "history" => {
                    reply["thread"]["turns"] = serde_json::json!([{"id":"unexpected-turn"}])
                }
                "model" => reply["model"] = serde_json::json!("other-model"),
                "cwd" => reply["cwd"] = serde_json::json!("/tmp/another"),
                "network" => reply["sandbox"]["networkAccess"] = serde_json::json!(true),
                _ => unreachable!(),
            }
            assert!(coordinator.initialize_thread(Uuid::now_v7()).is_err());
            assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
            assert_eq!(coordinator.thread_id(), Some("caller-thread"));
            assert!(
                !coordinator
                    .journal
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, JournalEntry::ThreadBound { .. }))
            );
        }
    }

    #[test]
    fn caller_idle_guard_and_duplicate_notification_fail_closed() {
        let mut coordinator = caller_coordinator();
        assert!(
            coordinator
                .set_writer_access(true, 7, Uuid::now_v7())
                .is_err()
        );
        assert!(
            !coordinator
                .server
                .calls
                .iter()
                .any(|(method, _)| method == "thread/settings/update")
        );
        coordinator.initialize_thread(Uuid::now_v7()).unwrap();
        prepend_caller_quiescence(&mut coordinator.server);
        coordinator
            .server
            .replies
            .push_back(Ok(serde_json::json!({})));
        coordinator
            .server
            .messages
            .push_back(settings_notification(true));
        coordinator
            .set_writer_access(true, 7, Uuid::now_v7())
            .unwrap();
        assert!(coordinator.ingest(settings_notification(true)).is_err());
        assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
        let before = coordinator.server.calls.len();
        assert!(
            coordinator
                .set_writer_access(false, 7, Uuid::now_v7())
                .is_err()
        );
        assert_eq!(coordinator.server.calls.len(), before);
    }

    #[test]
    fn caller_release_with_loaded_child_retains_prior_writer_selection() {
        let mut coordinator = caller_coordinator();
        coordinator.initialize_thread(Uuid::now_v7()).unwrap();
        prepend_caller_quiescence(&mut coordinator.server);
        coordinator
            .server
            .replies
            .push_back(Ok(serde_json::json!({})));
        coordinator
            .server
            .messages
            .push_back(settings_notification(true));
        coordinator
            .set_writer_access(true, 7, Uuid::now_v7())
            .unwrap();
        let journal = coordinator.journal.entries.clone();
        let effects = coordinator
            .server
            .calls
            .iter()
            .filter(|(method, _)| method == "thread/settings/update")
            .count();
        coordinator.server.replies.push_back(Ok(serde_json::json!({
            "data":["caller-thread", "writable-child"], "nextCursor":null,
        })));
        assert!(
            coordinator
                .set_writer_access(false, 7, Uuid::now_v7())
                .is_err()
        );
        assert_eq!(coordinator.state(), &CoordinatorState::Idle);
        assert_eq!(coordinator.writer_generation, Some(7));
        assert_eq!(coordinator.sandbox, "workspace-write");
        assert_eq!(coordinator.approval_policy, "on-request");
        assert_eq!(coordinator.journal.entries, journal);
        assert_eq!(
            coordinator
                .server
                .calls
                .iter()
                .filter(|(method, _)| method == "thread/settings/update")
                .count(),
            effects
        );
    }

    #[test]
    fn caller_recovery_keeps_original_thread_and_drops_prior_write_selection() {
        let mut coordinator = caller_coordinator();
        coordinator.initialize_thread(Uuid::now_v7()).unwrap();
        coordinator.sandbox = "workspace-write".to_owned();
        coordinator.approval_policy = "on-request".to_owned();
        coordinator.writer_generation = Some(7);
        coordinator.server.replies.push_back(Ok(serde_json::json!({
            "thread":{"id":"caller-thread", "turns":[]}, "model":"gpt-5",
            "cwd":"/tmp/workspace", "approvalPolicy":"never",
            "sandbox":turn_sandbox("read-only", Path::new("/tmp/workspace")),
        })));
        coordinator
            .complete_connection_startup(RunLifecycle::Idle)
            .unwrap();
        assert_eq!(coordinator.thread_id(), Some("caller-thread"));
        assert_eq!(coordinator.sandbox, "read-only");
        assert!(coordinator.writer_generation.is_none());
        let params = &coordinator
            .server
            .calls
            .iter()
            .find(|(method, _)| method == "thread/resume")
            .unwrap()
            .1;
        assert_eq!(params["sandbox"], "read-only");
        assert_eq!(params["config"]["project_doc_max_bytes"], 0);
        assert!(
            !coordinator
                .server
                .calls
                .iter()
                .any(|(method, _)| method == "turn/start")
        );
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
    fn accepted_legacy_refusal_retry_replays_without_relaxing_unresolved_fences() {
        for rejection_count in [1, 2, 3] {
            for reject_thread in [true, false] {
                let mut server = FakeServer::default();
                if !reject_thread {
                    server
                        .replies
                        .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
                }
                for _ in 0..rejection_count {
                    server.replies.push_back(Err(TurnError::AppServerRejected {
                        code: -32000,
                        message: "refused".to_owned(),
                    }));
                }
                if reject_thread {
                    server
                        .replies
                        .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
                }
                server
                    .replies
                    .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
                let mut original = coordinator(server);
                for _ in 0..rejection_count {
                    assert!(matches!(
                        original.start_turn(request("legacy-retry", DeliveryMode::Submit)),
                        Err(TurnError::AppServerRejected { .. })
                    ));
                }
                let accepted = original
                    .start_turn(request("legacy-retry", DeliveryMode::Submit))
                    .unwrap();
                // This is the actual old journal shape: the refusal had no record.
                let records: Vec<_> = original
                    .journal
                    .entries
                    .into_iter()
                    .filter(|entry| !matches!(entry, JournalEntry::SubmitRefusal(_)))
                    .map(|entry| journal_record(entry).unwrap())
                    .collect();
                let retained_bytes = serde_json::to_vec(&records).unwrap();
                let mut restored = coordinator(FakeServer::default());
                restored.restore_turn_receipts(records.clone()).unwrap();
                let calls_before_replay = restored.server.calls.len();
                let replay = restored
                    .start_turn(request("legacy-retry", DeliveryMode::Submit))
                    .unwrap();
                assert!(replay.replayed);
                assert_eq!(replay.turn_id, accepted.turn_id);
                assert_eq!(restored.server.calls.len(), calls_before_replay);
                assert_eq!(serde_json::to_vec(&records).unwrap(), retained_bytes);
                let mut old_acceptance = records.clone();
                for (kind, payload) in &mut old_acceptance {
                    if *kind == AuditKind::TurnStarted {
                        payload.as_object_mut().unwrap().remove("effort");
                    }
                }
                let mut without_receipt = coordinator(FakeServer::default());
                without_receipt
                    .restore_turn_receipts(old_acceptance.clone())
                    .unwrap();
                assert!(matches!(
                    without_receipt.accept(request("legacy-retry", DeliveryMode::Submit)),
                    Err(TurnError::OutcomeUnknown)
                ));
                let terminal = TerminalTurn {
                    thread_id: accepted.thread_id.clone(),
                    turn_id: accepted.turn_id.clone(),
                    status: "completed".to_owned(),
                    effort: "medium".to_owned(),
                    final_response: None,
                    usage: None,
                };
                old_acceptance.push((
                    AuditKind::TurnTerminal,
                    serde_json::to_value(&terminal).unwrap(),
                ));
                let mut with_terminal = coordinator(FakeServer::default());
                with_terminal.restore_turn_receipts(old_acceptance).unwrap();
                let calls_before_replay = with_terminal.server.calls.len();
                assert_eq!(
                    with_terminal
                        .accept(request("legacy-retry", DeliveryMode::Send))
                        .unwrap(),
                    DeliveryResult::Terminal(terminal)
                );
                assert_eq!(with_terminal.server.calls.len(), calls_before_replay);
                let unresolved: Vec<_> = records
                    .iter()
                    .filter(|(kind, _)| *kind != AuditKind::TurnStarted)
                    .cloned()
                    .collect();
                assert!(restore_turn_receipt_maps(unresolved).is_err());
                let mut changed = records.clone();
                let (_, second) = changed
                    .iter_mut()
                    .filter(|(kind, _)| *kind == AuditKind::IdempotencyReserved)
                    .nth(1)
                    .unwrap();
                second["request_sha256"] = serde_json::json!("b".repeat(64));
                assert!(restore_turn_receipt_maps(changed).is_err());
                for field in ["run_generation", "server_key", "server_epoch"] {
                    let mut drift = records.clone();
                    let second = &mut drift
                        .iter_mut()
                        .filter(|(kind, _)| *kind == AuditKind::IdempotencyReserved)
                        .nth(1)
                        .unwrap()
                        .1;
                    second[field] = if field == "server_key" {
                        serde_json::json!("c".repeat(64))
                    } else {
                        serde_json::json!(second[field].as_u64().unwrap() + 1)
                    };
                    assert!(restore_turn_receipt_maps(drift).is_err(), "{field}");
                }
                for operation in records
                    .iter()
                    .filter(|(kind, _)| *kind == AuditKind::IdempotencyReserved)
                    .map(|(_, payload)| payload["operation_id"].clone())
                {
                    let mut successor = records.clone();
                    successor.insert(
                        1,
                        (
                            AuditKind::TurnIntent,
                            serde_json::json!({
                                "schema_version":1,"operation_ref":operation
                            }),
                        ),
                    );
                    assert!(restore_turn_receipt_maps(successor).is_err());
                }
                // Even a missing-effort acceptance cannot authorize another dispatch.
                let mut after_acceptance = records.clone();
                let accepted_payload = after_acceptance
                    .iter_mut()
                    .find(|(kind, _)| *kind == AuditKind::TurnStarted)
                    .unwrap()
                    .1
                    .as_object_mut()
                    .unwrap();
                accepted_payload.remove("effort");
                let mut extra_acceptance = serde_json::Value::Object(accepted_payload.clone());
                let mut extra_intent: OperationIntent = serde_json::from_value(
                    records
                        .iter()
                        .rev()
                        .find(|(kind, _)| *kind == AuditKind::IdempotencyReserved)
                        .unwrap()
                        .1
                        .clone(),
                )
                .unwrap();
                extra_intent.operation_id = Uuid::now_v7();
                extra_acceptance["operation_id"] = serde_json::json!(extra_intent.operation_id);
                extra_acceptance["turn_id"] = serde_json::json!("turn-extra");
                after_acceptance.push(journal_record(JournalEntry::Intent(extra_intent)).unwrap());
                assert!(restore_turn_receipt_maps(after_acceptance.clone()).is_err());
                after_acceptance.push((AuditKind::TurnStarted, extra_acceptance));
                assert!(restore_turn_receipt_maps(after_acceptance).is_err());
            }
        }
    }

    #[test]
    fn definitive_submit_refusal_releases_only_the_original_key_durably() {
        for (reject_thread, restart) in [(true, false), (true, true), (false, false), (false, true)]
        {
            let mut server = FakeServer::default();
            if !reject_thread {
                server
                    .replies
                    .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
            }
            server.replies.push_back(Err(TurnError::AppServerRejected {
                code: -32000,
                message: "refused".to_owned(),
            }));
            let mut original = coordinator(server);
            assert!(matches!(
                original.start_turn(request("refused", DeliveryMode::Submit)),
                Err(TurnError::AppServerRejected { .. })
            ));
            let records = original
                .journal
                .entries
                .clone()
                .into_iter()
                .map(journal_record)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                records.last().unwrap().1["schema"],
                "dolgorae.submit-refusal/v1"
            );
            let mut retry = if restart {
                let mut restored = coordinator(FakeServer::default());
                restored.restore_turn_receipts(records.clone()).unwrap();
                restored
            } else {
                original
            };
            let before = retry.server.calls.len();
            let mut changed = request("refused", DeliveryMode::Submit);
            changed.message.push('!');
            assert!(matches!(
                retry.start_turn(changed),
                Err(TurnError::IdempotencyConflict { .. })
            ));
            assert_eq!(retry.server.calls.len(), before);
            if retry.thread_id.is_none() {
                retry
                    .server
                    .replies
                    .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
            }
            retry
                .server
                .replies
                .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
            let accepted = retry
                .start_turn(request("refused", DeliveryMode::Submit))
                .unwrap();
            assert_eq!(
                retry
                    .start_turn(request("refused", DeliveryMode::Submit))
                    .unwrap()
                    .turn_id,
                accepted.turn_id
            );
            assert_eq!(
                retry.server.calls[before..]
                    .iter()
                    .filter(|(method, _)| method == "turn/start")
                    .count(),
                1
            );
            let retry_records = retry
                .journal
                .entries
                .into_iter()
                .map(|entry| journal_record(entry).unwrap());
            let accepted_records = if restart {
                records.into_iter().chain(retry_records).collect()
            } else {
                retry_records.collect()
            };
            assert!(
                replay_durable_submit(
                    accepted_records,
                    &request("refused", DeliveryMode::Submit),
                    "medium"
                )
                .unwrap()
                .is_some()
            );
        }
    }

    #[test]
    fn a_refusal_cannot_resolve_an_earlier_unknown_or_different_operation() {
        let operation = Uuid::now_v7();
        let intent = OperationIntent {
            operation_id: operation,
            idempotency_key: "key".to_owned(),
            request_sha256: normalized_request_digest(
                &request("key", DeliveryMode::Submit),
                "medium",
            )
            .unwrap(),
            run_generation: 1,
            server_key: "a".repeat(64),
            server_epoch: 7,
            provisional_thread: true,
            accepted_input: None,
        };
        for mutation in ["operation", "key", "digest", "duplicate", "later"] {
            let mut refusal = crate::operation::SubmitRefusal::new(
                operation,
                intent.idempotency_key.clone(),
                intent.request_sha256.clone(),
            );
            match mutation {
                "operation" => refusal.operation_id = Uuid::now_v7(),
                "key" => refusal.idempotency_key.push('!'),
                "digest" => refusal.request_sha256 = "b".repeat(64),
                _ => (),
            }
            let mut records = vec![journal_record(JournalEntry::Intent(intent.clone())).unwrap()];
            if mutation == "later" {
                let mut later = intent.clone();
                later.operation_id = Uuid::now_v7();
                records.push(journal_record(JournalEntry::Intent(later)).unwrap());
            }
            records.push(journal_record(JournalEntry::SubmitRefusal(refusal.clone())).unwrap());
            if mutation == "duplicate" {
                records.push(journal_record(JournalEntry::SubmitRefusal(refusal)).unwrap());
            }
            assert!(restore_turn_receipt_maps(records).is_err(), "{mutation}");
        }
    }

    #[test]
    fn refusal_sync_failure_retains_the_reservation_and_fences_retry() {
        #[derive(Default)]
        struct FailingResolution {
            entries: Vec<JournalEntry>,
        }
        impl DurableTurnJournal for FailingResolution {
            fn append_and_sync(&mut self, entry: JournalEntry) -> Result<(), TurnError> {
                if matches!(entry, JournalEntry::SubmitRefusal(_)) {
                    return Err(TurnError::Journal(
                        "injected refusal sync failure".to_owned(),
                    ));
                }
                self.entries.push(entry);
                Ok(())
            }
        }
        let mut server = FakeServer::default();
        server.replies.push_back(Err(TurnError::AppServerRejected {
            code: -32000,
            message: "refused".to_owned(),
        }));
        let mut original = coordinator_with_journal(
            server,
            ThreadAttach::Start,
            Box::new(RecordingDiagnostics::default()),
            SessionSafetyPolicy::Standard,
            None,
            FailingResolution::default(),
        );
        assert!(matches!(
            original.start_turn(request("key", DeliveryMode::Submit)),
            Err(TurnError::Journal(_))
        ));
        assert!(matches!(
            original.start_turn(request("key", DeliveryMode::Submit)),
            Err(TurnError::OutcomeUnknown)
        ));
        let mut restored = coordinator(FakeServer::default());
        restored
            .restore_turn_receipts(
                original
                    .journal
                    .entries
                    .into_iter()
                    .map(|entry| journal_record(entry).unwrap())
                    .collect(),
            )
            .unwrap();
        assert!(matches!(
            restored.start_turn(request("key", DeliveryMode::Submit)),
            Err(TurnError::OutcomeUnknown)
        ));
        assert!(
            !restored
                .server
                .calls
                .iter()
                .any(|(method, _)| method == "thread/start" || method == "turn/start")
        );
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

    fn successor_request(method: &str) -> Value {
        let params = if method == "item/permissions/requestApproval" {
            serde_json::json!({"threadId":"thread-1","turnId":"turn-1","itemId":"permission","cwd":std::fs::canonicalize(std::env::temp_dir()).unwrap(),"permissions":{"fileSystem":{"read":["file.txt"]}}})
        } else {
            serde_json::json!({"threadId":"thread-1","turnId":"turn-1","serverName":"fixture","mode":"form","message":"private request canary","requestedSchema":{"type":"object","properties":{"q":{"type":"string","enum":["yes","no"]}}}})
        };
        serde_json::json!({"id":79,"method":method,"params":params})
    }

    #[test]
    fn public_resolution_owns_atomic_identity_lookup_and_replay() {
        struct ScopedJournal(MemoryJournal);
        impl DurableTurnJournal for ScopedJournal {
            fn append_and_sync(&mut self, e: JournalEntry) -> Result<(), TurnError> {
                self.0.append_and_sync(e)
            }
            fn workspace_ref(&self) -> Option<&str> {
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            }
        }
        for protected in [false, true] {
            let mut server = FakeServer::default();
            server
                .replies
                .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
            server
                .replies
                .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
            let mut c = coordinator_with_journal(
                server,
                ThreadAttach::Start,
                Box::new(RecordingDiagnostics::default()),
                SessionSafetyPolicy::Standard,
                None,
                ScopedJournal(MemoryJournal::default()),
            );
            c.set_acceptance_controller(crate::domain::ControllerIdentity {
                controller_id: c.controller_id,
                kind: crate::domain::ControllerKind::HumanCli,
                instance_id: "public-test".into(),
                subject_id: None,
                generation: 1,
            });
            c.start_turn(request("turn", DeliveryMode::Submit)).unwrap();
            c.ingest(serde_json::json!({"id":79,"method":"item/tool/requestUserInput","params":{"threadId":"thread-1","turnId":"turn-1","questions":[{"id":"q","header":"Q","question":"Choose","isSecret":protected,"isOther":true}]}})).unwrap();
            let intent = crate::public_resolution::Intent {
                schema: crate::public_resolution::INTENT_SCHEMA.into(),
                body: crate::public_resolution::Body {
                    run_ref: c.run_id.to_string(),
                    interaction_ref: c.public_request_ids[&79].to_string(),
                    turn_ref: "turn-1".into(),
                    thread_generation: c.thread_generation,
                    decision: "accept_once".into(),
                    granted_permissions: vec![],
                    protected_input_present: false,
                    answers: if protected {
                        vec![]
                    } else {
                        vec![crate::public_resolution::Answer {
                            question_id: "q".into(),
                            selected_labels: vec!["choice".into()],
                            text: None,
                        }]
                    },
                },
            };
            let mut wrong_decision = intent.clone();
            wrong_decision.body.decision = "decline".into();
            assert!(matches!(
                c.respond_public(
                    79,
                    "invalid-decision".into(),
                    serde_json::json!({"answers":{"q":{"answers":["choice"]}}}),
                    wrong_decision
                ),
                Err(TurnError::InteractionResponseInvalid { .. })
            ));
            let receipt = c
                .respond_public(
                    79,
                    "answer".into(),
                    serde_json::json!({"answers":{"q":{"answers":["choice"]}}}),
                    intent.clone(),
                )
                .unwrap()
                .unwrap();
            assert_eq!(c.server.responses.len(), 1);
            let stored = c.public_resolutions[0].clone();
            assert_eq!(stored.operation_ref, receipt);
            assert_eq!(stored.contains_protected_input, protected);
            let mut replacement = intent.clone();
            replacement.body.answers = vec![crate::public_resolution::Answer {
                question_id: "q".into(),
                selected_labels: vec![],
                text: Some("choice".into()),
            }];
            if protected {
                assert_eq!(
                    c.replay_public_response(&replacement, "answer").unwrap(),
                    Some(receipt)
                );
            } else {
                assert!(matches!(
                    c.replay_public_response(&replacement, "answer"),
                    Err(TurnError::IdempotencyConflict { .. })
                ));
            }
            c.ingest(serde_json::json!({"id":80,"method":"item/tool/requestUserInput","params":{"threadId":"thread-1","turnId":"turn-1","questions":[{"id":"q","header":"Q","question":"Choose","isSecret":protected,"isOther":true}]}})).unwrap();
            let mut other = intent.clone();
            other.body.interaction_ref = c.public_request_ids[&80].to_string();
            assert!(matches!(
                c.replay_public_response(&other, "answer"),
                Err(TurnError::IdempotencyConflict { .. })
            ));
            let decisions = c
                .journal
                .0
                .entries
                .iter()
                .filter_map(|e| {
                    if matches!(e, JournalEntry::ApprovalDecided { .. }) {
                        Some(journal_record(e.clone()).unwrap().1)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(decisions.len(), 1);
            assert_eq!(
                decisions[0]["public_resolution"]["operation_ref"],
                receipt.to_string()
            );
            if protected {
                assert!(decisions[0]["response_sha256"].is_null());
                assert!(!decisions[0].to_string().contains("choice"));
            }
            let mut records = Vec::new();
            for e in &c.journal.0.entries {
                let (kind, payload) = journal_record(e.clone()).unwrap();
                records.push(
                    crate::audit::AuditRecord::new(
                        records.len() as u64 + 1,
                        "2026-10-01T00:00:00.000000Z",
                        c.run_id,
                        1,
                        kind,
                        crate::jcs::parse(&payload.to_string()).unwrap(),
                        "0".repeat(64),
                    )
                    .unwrap(),
                );
            }
            let lookup =
                crate::public_resolution::lookup(&records, &stored.mutation_envelope.namespace)
                    .unwrap();
            assert_eq!(lookup["state"], "accepted");
            assert_eq!(lookup["current_outcome"], "unknown");
            assert_eq!(
                lookup["accepted_subjects"]["interaction_ref"],
                intent.body.interaction_ref
            );
            records.push(
                crate::audit::AuditRecord::new(
                    records.len() as u64 + 1,
                    "2026-10-01T00:00:01.000000Z",
                    c.run_id,
                    1,
                    AuditKind::TurnTerminal,
                    crate::jcs::parse(r#"{"turn_id":"turn-1"}"#).unwrap(),
                    "0".repeat(64),
                )
                .unwrap(),
            );
            assert_eq!(
                crate::public_resolution::lookup(&records, &stored.mutation_envelope.namespace)
                    .unwrap()["current_outcome"],
                "completed"
            );
            c.public_resolutions.clear();
            c.resolved_interactions.clear();
            c.restore_interaction_resolutions(decisions.clone())
                .unwrap();
            assert_eq!(
                c.replay_public_response(&intent, "answer").unwrap(),
                Some(receipt)
            );
            let mut legacy = decisions[0].clone();
            legacy.as_object_mut().unwrap().remove("public_resolution");
            let legacy = crate::audit::AuditRecord::new(
                1,
                "2026-10-01T00:00:00.000000Z",
                c.run_id,
                1,
                AuditKind::ApprovalDecided,
                crate::jcs::parse(&legacy.to_string()).unwrap(),
                "0".repeat(64),
            )
            .unwrap();
            assert_eq!(
                crate::public_resolution::lookup(&[legacy], &stored.mutation_envelope.namespace)
                    .unwrap()["state"],
                "unresolved"
            );
        }
    }

    #[test]
    fn successor_interactions_have_real_decisions_and_protected_receipt_replay() {
        for method in [
            "item/permissions/requestApproval",
            "mcpServer/elicitation/request",
        ] {
            for decision in ["accept_once", "decline", "cancel"] {
                let mut c = running_turn_with(&RecordingDiagnostics::default(), vec![]);
                c.cwd = std::fs::canonicalize(std::env::temp_dir()).unwrap();
                c.enable_successor_interactions(true);
                c.ingest(successor_request(method)).unwrap();
                assert_eq!(c.state(), &CoordinatorState::WaitingInteraction);
                let body = if method == "item/permissions/requestApproval" {
                    serde_json::json!({"decision":decision,"granted_permissions":if decision=="accept_once" {vec!["filesystem_read:file.txt"]} else {vec![]}})
                } else {
                    serde_json::json!({"decision":decision,"answers":if decision=="accept_once" {serde_json::json!({"q":"yes"})} else {serde_json::json!({})}})
                };
                let receipt = c.respond(79, "original".into(), body).unwrap().unwrap();
                assert_eq!(c.state(), &CoordinatorState::Running);
                assert_eq!(c.server.responses.len(), 1);
                assert_eq!(
                    c.respond(
                        79,
                        "original".into(),
                        serde_json::json!({"replacement":"secret canary"})
                    )
                    .unwrap(),
                    Some(receipt)
                );
                assert!(matches!(
                    c.respond(79, "other".into(), serde_json::json!({})),
                    Err(TurnError::InteractionAlreadyResolved { .. })
                ));
                let decided = c
                    .journal
                    .entries
                    .iter()
                    .find_map(|v| {
                        if let JournalEntry::ApprovalDecided {
                            response_sha256,
                            resolution,
                            ..
                        } = v
                        {
                            assert!(response_sha256.is_none());
                            Some(resolution)
                        } else {
                            None
                        }
                    })
                    .unwrap();
                assert_eq!(decided["decision"], decision);
                assert_eq!(decided["outcome"], "protected_decision");
                assert!(!decided.to_string().contains("yes"));
                assert_eq!(c.active_turn_id(), Some("turn-1"));
            }
        }
    }

    #[test]
    fn successor_ownerless_and_readonly_requests_decline_during_ingestion() {
        for method in [
            "item/permissions/requestApproval",
            "mcpServer/elicitation/request",
        ] {
            for fixed in [false, true] {
                let mut c = running_turn_with(&RecordingDiagnostics::default(), vec![]);
                c.cwd = std::fs::canonicalize(std::env::temp_dir()).unwrap();
                c.enable_successor_interactions(fixed);
                if fixed {
                    c.safety_policy = SessionSafetyPolicy::FixedReadOnly;
                }
                c.ingest(successor_request(method)).unwrap();
                assert!(c.pending.is_empty());
                assert_eq!(c.state(), &CoordinatorState::Running);
                assert_eq!(c.active_turn_id(), Some("turn-1"));
                assert_eq!(c.server.responses.len(), 1);
                assert!(c.journal.entries.iter().any(|e|matches!(e,JournalEntry::ApprovalDecided{resolution,..} if resolution["decision"]=="decline" && resolution["policy_cause"]==if fixed {"READ_ONLY_POLICY"} else {"SPECIALIST_INTERACTION_UNSUPPORTED"})));
                assert!(!c.journal.entries.iter().any(|e| matches!(
                    e,
                    JournalEntry::Terminal { .. } | JournalEntry::WriterAccessChanged { .. }
                )));
            }
        }
    }

    #[test]
    fn successor_rejects_foreign_requests_and_unrequested_grants() {
        let mut c = running_turn_with(&RecordingDiagnostics::default(), vec![]);
        c.cwd = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        c.enable_successor_interactions(true);
        let mut foreign = successor_request("item/permissions/requestApproval");
        foreign["params"]["threadId"] = serde_json::json!("foreign");
        c.ingest(foreign).unwrap();
        assert!(c.server.responses.is_empty());
        c.ingest(successor_request("item/permissions/requestApproval"))
            .unwrap();
        assert!(c.respond(79,"bad".into(),serde_json::json!({"decision":"accept_once","granted_permissions":["filesystem_write:file.txt"]})).is_err());
        assert!(c.server.responses.is_empty());
    }

    #[test]
    fn activity_producers_drop_command_and_runtime_bodies() {
        let mut c = running_turn_with(&RecordingDiagnostics::default(), vec![]);
        for completed in [false, true] {
            c.ingest(serde_json::json!({"method":if completed {"item/completed"} else {"item/started"},"params":{"threadId":"thread-1","turnId":"turn-1","item":{"type":"commandExecution","id":"command","command":"private command canary","aggregatedOutput":"private output canary","exitCode":3}}})).unwrap();
        }
        c.ingest(serde_json::json!({"method":"error","params":{"threadId":"thread-1","turnId":"turn-1","error":{"message":"private error canary"}}})).unwrap();
        let activities = c
            .journal
            .entries
            .iter()
            .filter_map(|v| {
                if let JournalEntry::SafeActivity { data, .. } = v {
                    Some(data)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(activities.len(), 3);
        let encoded = serde_json::to_string(&activities).unwrap();
        assert!(!encoded.contains("canary"));
        assert!(encoded.contains("[redacted]"));
        assert!(encoded.contains("runtime.error"));
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
    fn native_absolute_changes_and_moves_remain_within_the_workspace() {
        let workspace = Path::new("/tmp/workspace");
        let changes = serde_json::json!([
            {"path":"/tmp/workspace/new.txt","kind":{"type":"add"},"diff":"+new"},
            {"path":"/tmp/workspace/deleted.txt","kind":{"type":"delete"},"diff":"-old"},
            {"path":"/tmp/workspace/old.txt","kind":{"type":"update","move_path":"/tmp/workspace/nested/new.txt"},"diff":"-old\n+new"}
        ]);
        let normalized = normalize_file_changes(changes.as_array().unwrap(), workspace).unwrap();
        assert_eq!(normalized[0].path, LosslessPath::Utf8("new.txt".to_owned()));
        assert_eq!(
            normalized[1].path,
            LosslessPath::Utf8("deleted.txt".to_owned())
        );
        assert_eq!(
            normalized[2].move_path,
            Some(LosslessPath::Utf8("nested/new.txt".to_owned()))
        );
        for path in [
            "/tmp/other/file",
            "/tmp/workspace-sibling/file",
            "/tmp/workspace/../other/file",
            "../file",
            "/tmp/workspace",
            ".",
        ] {
            let invalid = serde_json::json!([{"path":path,"kind":{"type":"add"},"diff":"+new"}]);
            assert!(matches!(
                normalize_file_changes(invalid.as_array().unwrap(), workspace),
                Err(TurnError::CorrelationMismatch)
            ));
            let invalid_move = serde_json::json!([{"path":"old","kind":{"type":"update","move_path":path},"diff":"+new"}]);
            assert!(matches!(
                normalize_file_changes(invalid_move.as_array().unwrap(), workspace),
                Err(TurnError::CorrelationMismatch)
            ));
        }
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
            (
                "thread-1",
                Value::String("partial_answer".to_owned()),
                "partial",
            ),
        ] {
            server.messages.push_back(serde_json::json!({"method":"item/completed","params":{"threadId":thread,"turnId":"turn-1","item":{"type":"agentMessage","status":"completed","phase":phase,"text":text}}}));
        }
        server.replies.push_back(Ok(serde_json::json!({"thread":{"turns":[{"id":"turn-1","items":[
            {"type":"agentMessage","status":"completed","phase":null,"text":"fallback"},
            {"type":"agentMessage","status":"completed","phase":"commentary","text":"commentary"},
            {"type":"agentMessage","status":"completed","phase":"final_answer","text":"final"},
            {"type":"agentMessage","status":"completed","phase":"partial_answer","text":"partial"}
        ]}]}})));
        server.messages.push_back(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","usage":{"inputTokens":3}}}}));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        for _ in 0..5 {
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
    fn artifact_quota_retains_terminal_status_and_original_response_digest() {
        struct QuotaStore {
            failure_sha256: String,
            inner: MemoryArtifactStore,
        }
        impl ResponseArtifactStore for QuotaStore {
            fn store(&mut self, bytes: &[u8]) -> Result<StoredArtifact, TurnError> {
                if sha256_hex(bytes) == self.failure_sha256 {
                    Err(TurnError::ArtifactQuotaExceeded)
                } else {
                    self.inner.store(bytes)
                }
            }
        }
        let text = "x".repeat(MAX_INLINE_FINAL_RESPONSE_BYTES + 1);
        let expected_hash = sha256_hex(text.as_bytes());
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        server.messages.push_back(serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[{"type":"agentMessage","status":"completed","phase":"final_answer","text":text}]}}}));
        let mut coordinator = coordinator_with_journal_and_artifacts(
            server,
            ThreadAttach::Start,
            Box::new(RecordingDiagnostics::default()),
            SessionSafetyPolicy::Standard,
            None,
            MemoryJournal::default(),
            QuotaStore {
                failure_sha256: expected_hash.clone(),
                inner: MemoryArtifactStore::default(),
            },
        );
        coordinator
            .start_turn(request("original-quota", DeliveryMode::Submit))
            .unwrap();
        let terminal = coordinator.next_event().unwrap().unwrap();
        assert_eq!(terminal.status, "completed");
        assert_eq!(
            terminal.final_response,
            Some(FinalResponse::Unavailable {
                byte_length: text.len() as u64,
                sha256: expected_hash,
                reason: "quota_exceeded".into(),
            })
        );
        assert!(
            coordinator
                .journal
                .entries
                .iter()
                .any(|entry| matches!(entry, JournalEntry::Terminal { .. }))
        );
        assert!(
            !coordinator
                .journal
                .entries
                .iter()
                .any(|entry| matches!(entry, JournalEntry::OutcomeUnknown { .. }))
        );
    }

    #[test]
    fn accepted_input_bounds_reject_before_turn_intent() {
        let mut coordinator = coordinator(FakeServer::default());
        let mut oversized = request("oversized-input", DeliveryMode::Submit);
        oversized.message = "x".repeat(8 * 1024 * 1024 + 1);
        assert!(matches!(
            coordinator.start_turn(oversized),
            Err(TurnError::InvalidInput(
                "message exceeds the 8 MiB request bound"
            ))
        ));
        assert!(coordinator.journal.entries.is_empty());

        let path = std::env::temp_dir().join(format!("dolgorae-input-image-{}", Uuid::now_v7()));
        std::fs::write(&path, b"image").unwrap();
        let image = ImageSnapshot::capture(&path, ImageDetail::Low).unwrap();
        let mut too_many_images = request("too-many-images", DeliveryMode::Submit);
        too_many_images.images = vec![image; 65];
        assert!(matches!(
            coordinator.start_turn(too_many_images),
            Err(TurnError::InvalidInput("at most 64 images are accepted"))
        ));
        assert!(coordinator.journal.entries.is_empty());
        std::fs::remove_file(path).unwrap();
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
    fn connection_startup_records_ready_before_the_first_turn_in_conformant_history() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        let mut coordinator = coordinator(server);
        coordinator.restore_durable_state(RunLifecycle::Starting, None, None, None);
        assert!(coordinator.journal.entries.is_empty());
        coordinator
            .complete_connection_startup(RunLifecycle::Starting)
            .unwrap();
        assert_eq!(coordinator.server.calls.len(), 2);
        coordinator
            .start_turn(request("first-turn", DeliveryMode::Submit))
            .unwrap();

        let run_id = coordinator.run_id;
        let bootstrap = [
            (
                AuditKind::WorkspaceInitialized,
                serde_json::json!({"workspace_id":"a".repeat(64)}),
            ),
            (
                AuditKind::IdempotencyReserved,
                serde_json::to_value(crate::conformance::IdempotencyIntent {
                    schema_version: 1,
                    operation: crate::conformance::IdempotencyOperation::StartRun,
                    idempotency_key: "start".to_owned(),
                    normalized_identity_sha256: "b".repeat(64),
                    run_id,
                })
                .unwrap(),
            ),
            (
                AuditKind::RunCreated,
                serde_json::json!({"initial_access":"read"}),
            ),
        ];
        let runtime = coordinator
            .journal
            .entries
            .into_iter()
            .map(journal_record)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for include_readiness in [false, true] {
            let mut previous = crate::audit::GENESIS_PREVIOUS_HASH.to_owned();
            let mut records = Vec::new();
            for (kind, payload) in bootstrap
                .iter()
                .chain(runtime.iter().skip(usize::from(!include_readiness)))
            {
                let record = crate::audit::AuditRecord::new(
                    records.len() as u64 + 1,
                    "2026-09-30T00:00:00.000000Z",
                    run_id,
                    u64::from(records.len() >= bootstrap.len()),
                    *kind,
                    parse(&serde_json::to_string(payload).unwrap()).unwrap(),
                    &previous,
                )
                .unwrap();
                previous = record.hash().to_owned();
                records.push(record);
            }
            let report = crate::conformance::verify_ledger_records(run_id, &records);
            if include_readiness {
                assert_eq!(report.unwrap().lifecycle, RunLifecycle::Running);
            } else {
                assert!(report.is_err());
            }
        }
    }

    #[test]
    fn caller_retained_thread_startup_keeps_original_refusal_without_publishing_a_binding() {
        for (field, predicate) in [
            ("cwd", "thread_settings_cwd"),
            ("approvalPolicy", "thread_settings_approval_policy"),
            ("sandbox", "thread_settings_sandbox_type"),
            ("network", "thread_settings_network_access"),
            ("model", "thread_model"),
        ] {
            let mut coordinator = caller_coordinator();
            coordinator.initialize_thread(Uuid::now_v7()).unwrap();
            coordinator.journal.entries.clear();
            let mut response = serde_json::json!({
                "thread":{"id":"caller-thread", "turns":[]}, "model":"gpt-5",
                "cwd":"/tmp/workspace", "approvalPolicy":"never",
                "sandbox":turn_sandbox("read-only", Path::new("/tmp/workspace")),
            });
            if field == "network" {
                response["sandbox"]["networkAccess"] = Value::Bool(true);
            } else {
                response[field] = serde_json::json!("untrusted refusal content");
            }
            coordinator.server.replies.push_back(Ok(response));
            let error = coordinator
                .complete_connection_startup(RunLifecycle::Idle)
                .unwrap_err();
            assert_eq!(error, TurnError::CorrelationMismatch, "{field}");
            assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
            assert_eq!(coordinator.thread_id(), Some("caller-thread"));
            let diagnostic = coordinator.caller_startup_failure().unwrap();
            assert_eq!(diagnostic["substage"], "thread_settings");
            assert_eq!(diagnostic["original_error"]["kind"], "CorrelationMismatch");
            assert_eq!(diagnostic["refused_predicate"], predicate);
            assert!(!diagnostic.to_string().contains("untrusted refusal content"));
            assert!(coordinator.caller_startup_phase.is_none());
            assert!(
                !coordinator
                    .journal
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, JournalEntry::ThreadBound { .. }))
            );
            let writes = coordinator.server.calls.len();
            assert_eq!(
                coordinator.initialize_thread(Uuid::now_v7()),
                Err(TurnError::OutcomeUnknown)
            );
            assert_eq!(coordinator.server.calls.len(), writes);
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
        }
        let mut coordinator = caller_coordinator();
        coordinator.initialize_thread(Uuid::now_v7()).unwrap();
        coordinator.server.replies.push_back(Ok(serde_json::json!({
            "thread":{"id":"caller-thread", "turns":[]}, "model":"gpt-5",
            "cwd":"/tmp/workspace", "approvalPolicy":"never",
            "sandbox":turn_sandbox("read-only", Path::new("/tmp/workspace")),
        })));
        coordinator
            .complete_connection_startup(RunLifecycle::Idle)
            .unwrap();
        assert!(coordinator.caller_startup_phase.is_none());
        assert!(coordinator.caller_startup_failure().is_none());
        assert_eq!(coordinator.thread_id(), Some("caller-thread"));
    }

    #[test]
    fn retained_thread_rebinds_current_server_policy_without_submitting_a_turn() {
        for lifecycle in [RunLifecycle::Paused, RunLifecycle::Idle] {
            let mut server = FakeServer::default();
            server
                .replies
                .push_back(Ok(serde_json::json!({"thread":{"id":"thread-old"}})));
            let mut coordinator = coordinator(server);
            coordinator.server_epoch = 8;
            coordinator.restore_durable_state(
                lifecycle,
                Some("thread-old".to_owned()),
                None,
                Some("turn-terminal".to_owned()),
            );
            coordinator
                .restore_turn_receipts(vec![(
                    AuditKind::ThreadBound,
                    serde_json::json!({
                        "thread_id":"thread-old", "thread_generation":3, "effective_policy": {
                            "access":"read", "verification":"verified", "policy_epoch":10,
                            "thread_generation":3, "server_epoch":7, "writer_generation":null
                        }
                    }),
                )])
                .unwrap();
            if lifecycle == RunLifecycle::Paused {
                coordinator.resume().unwrap();
            } else {
                coordinator.complete_connection_startup(lifecycle).unwrap();
            }
            assert_eq!(coordinator.state(), &CoordinatorState::Idle);
            assert_eq!(coordinator.thread_id(), Some("thread-old"));
            assert_eq!(coordinator.server.calls.last().unwrap().0, "thread/resume");
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
            let JournalEntry::ThreadBound {
                thread_generation,
                effective_policy,
                ..
            } = &coordinator.journal.entries[1]
            else {
                panic!("resume must durably rebind the thread")
            };
            assert_eq!(*thread_generation, 4);
            assert_eq!(effective_policy.policy_epoch.0, 11);
            assert_eq!(effective_policy.server_epoch, Some(8));
            assert_eq!(effective_policy.thread_generation, Some(4));
            assert_eq!(effective_policy.access, crate::domain::Access::Unknown);
            assert_eq!(
                effective_policy.verification,
                crate::domain::PolicyVerification::Unverified
            );
            if lifecycle == RunLifecycle::Paused {
                assert!(matches!(
                    coordinator.journal.entries.last(),
                    Some(JournalEntry::LifecycleTransition {
                        previous: RunLifecycle::Paused,
                        current: RunLifecycle::Idle,
                        ..
                    })
                ));
            } else {
                assert!(
                    !coordinator
                        .journal
                        .entries
                        .iter()
                        .any(|entry| matches!(entry, JournalEntry::LifecycleTransition { .. }))
                );
            }
        }
    }

    #[test]
    fn resume_with_a_different_native_thread_never_publishes_idle() {
        for lifecycle in [RunLifecycle::Paused, RunLifecycle::Idle] {
            let mut server = FakeServer::default();
            server
                .replies
                .push_back(Ok(serde_json::json!({"thread":{"id":"thread-foreign"}})));
            let mut coordinator = coordinator(server);
            coordinator.restore_durable_state(lifecycle, Some("thread-old".to_owned()), None, None);
            let error = if lifecycle == RunLifecycle::Paused {
                coordinator.resume().unwrap_err()
            } else {
                coordinator
                    .complete_connection_startup(lifecycle)
                    .unwrap_err()
            };
            assert_eq!(error, TurnError::CorrelationMismatch);
            assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
            assert!(
                !coordinator
                    .server
                    .calls
                    .iter()
                    .any(|(method, _)| method == "turn/start")
            );
            assert!(!coordinator.journal.entries.iter().any(|entry| matches!(
                entry,
                JournalEntry::LifecycleTransition {
                    current: RunLifecycle::Idle,
                    ..
                }
            )));
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
    fn abandoned_live_turn_reconciles_only_its_terminal_history_without_replay() {
        let mut server = FakeServer::default();
        server
            .replies
            .push_back(Ok(serde_json::json!({"thread":{"id":"thread-1"}})));
        server
            .replies
            .push_back(Ok(serde_json::json!({"turn":{"id":"turn-1"}})));
        for status in ["inProgress", "completed"] {
            server.replies.push_back(Ok(serde_json::json!({
                "thread":{"id":"thread-1","turns":[{"id":"turn-1","status":status}]}
            })));
        }
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("original", DeliveryMode::Submit))
            .unwrap();
        coordinator.abandon().unwrap();
        assert_eq!(coordinator.active_turn_id(), None);
        assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
        let before = coordinator.journal().entries.len();
        assert_eq!(
            coordinator.reconcile_history(),
            Err(TurnError::CorrelationMismatch)
        );
        assert_eq!(coordinator.journal().entries.len(), before);
        assert_eq!(coordinator.state(), &CoordinatorState::OutcomeUnknown);
        assert!(coordinator.reconcile_history().unwrap());
        assert_eq!(coordinator.state(), &CoordinatorState::Paused);
        let reads = coordinator
            .server
            .calls
            .iter()
            .filter(|(method, _)| method == "thread/read")
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 2);
        assert!(reads.iter().all(|(_, params)| {
            params == &serde_json::json!({"threadId":"thread-1","includeTurns":true})
        }));
        assert_eq!(
            coordinator
                .server
                .calls
                .iter()
                .filter(|(method, _)| method == "turn/start")
                .count(),
            1
        );
        assert!(
            !coordinator.server.calls.iter().any(|(method, _)| {
                matches!(method.as_str(), "thread/resume" | "turn/interrupt")
            })
        );
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
    fn recovery_parent_requires_exact_null_across_every_split() {
        let prefix = br#"{"thread":{"id":"thread-1","parentThreadId":"#;
        let suffix =
            br#", "turns":[{"id":"turn-1","status":"completed","itemsView":"full","items":[]}]}}"#;
        for split in 0..=4 {
            let mut sink = ThreadTurnSink::for_recovery("thread-1", "turn-1");
            sink.accept(prefix).unwrap();
            sink.accept(&b"null"[..split]).unwrap();
            sink.accept(&b"null"[split..]).unwrap();
            sink.accept(suffix).unwrap();
            assert!(sink.into_recovery().unwrap().full);
        }
        for token in [
            "nope",
            "nul",
            "nullx",
            "\"null\"",
            "null x",
            "null\u{000b}",
            "null,\"parentThreadId\":null",
            "null,\"parentThread\\u0049d\":null",
            "null,\"extra\":0,\"extra\":0",
            "null,\"id\":\"thread-1\"",
        ] {
            let mut sink = ThreadTurnSink::for_recovery("thread-1", "turn-1");
            let bytes = [prefix, token.as_bytes(), suffix].concat();
            let received = bytes.chunks(1).try_for_each(|chunk| sink.accept(chunk));
            assert!(
                received.is_err() || sink.into_recovery().is_err(),
                "token={token:?}"
            );
        }
        let duplicate = br#"{"thread":{"id":"thread-1","parentThreadId":null},"thread":{"turns":[{"id":"turn-1","status":"completed","itemsView":"full","items":[]}]}}"#;
        let mut sink = ThreadTurnSink::for_recovery("thread-1", "turn-1");
        assert!(
            duplicate
                .chunks(1)
                .try_for_each(|chunk| sink.accept(chunk))
                .is_err()
        );
    }

    #[test]
    fn streamed_history_requires_one_complete_root_in_every_mode() {
        let valid = br#"{"thread":{"id":"thread-1","parentThreadId":null,"turns":[{"id":"turn-1","status":"completed","itemsView":"full","items":[]}]}}"#;
        let split_roots = br#"{"thread":{"id":"thread-1","parentThreadId":null}}
{"thread":{"turns":[{"id":"turn-1","status":"completed","itemsView":"full","items":[]}]}}"#;
        let cases = [
            ("split proof", split_roots.to_vec(), false),
            (
                "second root",
                [valid.as_slice(), b" \r\n\t {}"].concat(),
                false,
            ),
            ("scalar", [valid.as_slice(), b" true"].concat(), false),
            ("closer", [valid.as_slice(), b"}"].concat(), false),
            ("unclosed", valid[..valid.len() - 1].to_vec(), false),
            ("single root", valid.to_vec(), true),
            (
                "JSON whitespace",
                [b" \r\n\t ".as_slice(), valid, b" \t\r\n"].concat(),
                true,
            ),
            (
                "non-JSON whitespace",
                [valid.as_slice(), b"\x0b"].concat(),
                false,
            ),
            ("leading scalar", [b"0".as_slice(), valid].concat(), false),
            ("empty", Vec::new(), false),
        ];
        assert_history_partitions(&cases);
    }

    #[test]
    fn generation_release_history_binds_complete_terminal_bytes_and_original_thread() {
        fn boundary(bytes: &[u8]) -> Result<GenerationHistory, TurnError> {
            let mut sink = ThreadTurnSink::for_generation("original");
            for byte in bytes.chunks(1) {
                sink.accept(byte)?;
            }
            sink.into_generation()
        }
        let empty = br#"{"thread":{"id":"original","parentThreadId":null,"turns":[]}}"#;
        assert_eq!(boundary(empty).unwrap().turns, 0);
        let original=br#"{"thread":{"id":"original","parentThreadId":null,"turns":[{"id":"t1","status":"completed","items":[{"text":"original bytes"}]}]}}"#;
        let proof = boundary(original).unwrap();
        assert_eq!(proof.turns, 1);
        let changed = String::from_utf8(original.to_vec())
            .unwrap()
            .replace("original bytes", "changed bytes");
        assert_ne!(boundary(changed.as_bytes()).unwrap(), proof);
        let active = String::from_utf8(original.to_vec())
            .unwrap()
            .replace("completed", "inProgress");
        assert!(boundary(active.as_bytes()).is_err());
        for invalid in [br#"{"thread":{"id":"other","parentThreadId":null,"turns":[]}}"#.as_slice(),br#"{"thread":{"id":"original","parentThreadId":"parent","turns":[]}}"#,br#"{"thread":{"id":"original","parentThreadId":null}}"#,br#"{"thread":{"id":"original","parentThreadId":null,"turns":[{"id":"t","items":[]}]}}"#] {assert!(boundary(invalid).is_err());}
        let mut large = original.to_vec();
        let text = "x".repeat(2 * 1024 * 1024);
        large = String::from_utf8(large)
            .unwrap()
            .replace("original bytes", &text)
            .into_bytes();
        let mut sink = ThreadTurnSink::for_generation("original");
        sink.accept(&large).unwrap();
        assert!(sink.retained_parser_bytes() < 8192);
        assert_eq!(sink.into_generation().unwrap().turns, 1);
    }

    fn assert_history_partitions(cases: &[(&str, Vec<u8>, bool)]) {
        for mode in ["unbound", "bound", "recovery"] {
            for (case, bytes, expected) in cases {
                let partitions = (0..=bytes.len())
                    .map(|split| vec![&bytes[..split], &bytes[split..]])
                    .chain(std::iter::once(bytes.chunks(1).collect()));
                for chunks in partitions {
                    let chunk_count = chunks.len();
                    let mut sink = match mode {
                        "unbound" => ThreadTurnSink::new("turn-1"),
                        "bound" => ThreadTurnSink::new_bound("thread-1", "turn-1"),
                        _ => ThreadTurnSink::for_recovery("thread-1", "turn-1"),
                    };
                    let received = chunks.into_iter().try_for_each(|chunk| sink.accept(chunk));
                    let accepted = received.is_ok()
                        && if mode == "recovery" {
                            sink.into_recovery().is_ok_and(|history| history.full)
                        } else {
                            sink.into_items().is_ok_and(|items| items.is_empty())
                        };
                    assert_eq!(accepted, *expected, "{mode}: {case}; chunks={chunk_count}");
                }
            }
        }
    }

    #[test]
    fn streamed_history_rejects_invalid_grammar_in_every_mode() {
        let valid = br#"{"thread":{"id":"thread-1","parentThreadId":null,"turns":[{"id":"turn-1","status":"completed","itemsView":"full","items":[]}]}}"#;
        let trailing = br#"{"thread":{"id":"thread-1","parentThreadId":null,"turns":[{"id":"turn-1","status":"completed","itemsView":"full","items":[]}]},}"#;
        let unexpected = br#"{"thread":{"id":"thread-1","parentThreadId":null,"turns":[{"id":"turn-1","status":"completed","itemsView":"full","items":[]}]} true}"#;
        let control = [
            br#"{"extra":"a"#.as_slice(),
            b"\n",
            br#"b","#.as_slice(),
            &valid[1..],
        ]
        .concat();
        let unicode = [
            br#"{"extra":["\uD834\uDD1E",true,false,null,-12.5e+2],"#.as_slice(),
            &valid[1..],
        ]
        .concat();
        assert_history_partitions(&[
            ("trailing comma", trailing.to_vec(), false),
            ("unexpected literal", unexpected.to_vec(), false),
            ("raw control", control, false),
            ("complete grammar", unicode, true),
        ]);
    }

    #[test]
    fn streamed_history_grammar_matches_serde_json() {
        let seed = br#"{"thread":{"id":"thread-1","parentThreadId":null,"turns":[{"id":"turn-1","status":"completed","itemsView":"full","items":[{"type":"agentMessage","text":"answer"}]}]},"extra":{"flags":[true,false,null],"count":-12.5e+2,"label":"read"}}"#;
        let mut corpus = BTreeSet::new();
        corpus.insert(seed.to_vec());
        for index in 0..seed.len() {
            let mut deleted = seed.to_vec();
            deleted.remove(index);
            corpus.insert(deleted);
            for byte in 0..=u8::MAX {
                let mut replaced = seed.to_vec();
                replaced[index] = byte;
                corpus.insert(replaced);
            }
        }
        for index in 0..=seed.len() {
            for byte in 0..=u8::MAX {
                let mut inserted = seed.to_vec();
                inserted.insert(index, byte);
                corpus.insert(inserted);
            }
        }
        for extra in [
            r#"{"empty":[],"nested":[{},[0,-0,1,1.25,1e2,1E-2]],"literals":[true,false,null]}"#,
            r#"{"escapes":"\"\\\/\b\f\n\r\t\u0000\u00e9\uD834\uDD1E"}"#,
            "{\"utf8\":\"é한🦀\"}",
            r#"{"same":1,"same":2}"#,
            r#"{"trailing": [1,]}"#,
            r#"{"missing":}"#,
            r#"{"key" 1}"#,
            r#"{"unexpected":true false}"#,
            r#"{"escape":"\x"}"#,
            r#"{"escape":"\u12g4"}"#,
            r#"{"escape":"\uD800"}"#,
            r#"{"escape":"\uDC00"}"#,
            r#"{"escape":"\uD800\u0000"}"#,
        ] {
            corpus.insert(extra.as_bytes().to_vec());
            corpus.insert(
                [
                    &seed[..seed.len() - 1],
                    b",\"metadata\":",
                    extra.as_bytes(),
                    b"}",
                ]
                .concat(),
            );
        }
        for scalar in [
            "null", "true", "false", "-0", "123", "1.25e+2", "01", "1.", "-.1", "1e", "1e+", "1e-",
            "--1", "+1", "NaN", "Infinity", "True", "nul", "falsex",
        ] {
            corpus.insert(scalar.as_bytes().to_vec());
            corpus.insert(
                [
                    &seed[..seed.len() - 1],
                    b",\"scalar\":",
                    scalar.as_bytes(),
                    b"}",
                ]
                .concat(),
            );
        }
        for control in 0..=0x1f {
            corpus.insert([br#"{"text":"before"#.as_slice(), &[control], br#"after"}"#].concat());
        }
        for utf8 in [
            b"\xc0\xaf".as_slice(),
            b"\xe0\x80\x80",
            b"\xed\xa0\x80",
            b"\xf0\x80\x80\x80",
            b"\xf4\x90\x80\x80",
            b"\xf5\x80\x80\x80",
            b"\xc2",
            b"\xe2\x82",
        ] {
            corpus.insert([br#"{"text":""#.as_slice(), utf8, br#""}"#].concat());
        }
        for bytes in [
            b"".as_slice(),
            b" \t\r\n",
            b"{}{}",
            b"{} true",
            b"{}\x0b",
            b"[",
            b"{",
            b"[1,]",
            b"[1 2]",
        ] {
            corpus.insert(bytes.to_vec());
        }
        let mut mismatches = 0;
        let mut examples = Vec::new();
        for bytes in &corpus {
            let expected = serde_json::from_slice::<Value>(bytes).is_ok();
            // This is the sink's grammar layer; identity and duplicate-key
            // policy deliberately reject some otherwise valid JSON values.
            let mut grammar = JsonGrammar::default();
            let accepted = bytes.iter().try_for_each(|byte| grammar.push(*byte));
            let actual = accepted.is_ok() && grammar.finish().is_ok();
            if actual != expected {
                mismatches += 1;
                if examples.len() < 5 {
                    examples.push((bytes.clone(), expected, actual));
                }
            }
        }
        println!(
            "differential corpus={} seed_bytes={} mismatches={mismatches}",
            corpus.len(),
            seed.len()
        );
        assert_eq!(mismatches, 0, "{examples:?}");
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
