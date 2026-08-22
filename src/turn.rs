//! Thread and Turn lifecycle owned by one Run worker connection.

use crate::app_server::{JsonRpcConnection, SolicitedSink, TransportError, Wire};
use crate::audit::AuditKind;
use crate::fault::{FaultInjector, NoFaults};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::ledger::{Ledger, LedgerClock, SystemLedgerClock};
use crate::machine::MachineError;
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

pub const MAX_INLINE_FINAL_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_FINAL_RESPONSE_ARTIFACT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_INTERACTION_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TURN_ITEMS: usize = 16_384;
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
            Self::InteractionNotFound { request_id } => MachineError::new(
                "INTERACTION_NOT_FOUND",
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
    const fn as_str(self) -> &'static str {
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
    },
    TurnAccepted {
        operation_id: Uuid,
        thread_id: String,
        turn_id: String,
    },
    InteractionOpened {
        request_id: u64,
        method: String,
    },
    InteractionResolved {
        request_id: u64,
    },
    /// One Turn reached a terminal state.
    ///
    /// specs.md sends a Master to `run status.data.last_terminal` for the
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
        self.ledger
            .append_required_payload(kind, &payload, self.run_generation)
            .map_err(|error| TurnError::Journal(error.to_string()))
    }
}

/// One durable Ledger shared between the Turn writer and same-uid observers.
///
/// The lock is taken only for the length of one append, so an observer reading
/// the audit ledger is never blocked behind a Turn that is still running.
pub struct SharedLedgerJournal<C: LedgerClock = SystemLedgerClock, F: FaultInjector = NoFaults> {
    ledger: Arc<Mutex<Ledger<C, F>>>,
    run_generation: u64,
}

impl<C: LedgerClock, F: FaultInjector> SharedLedgerJournal<C, F> {
    #[must_use]
    pub const fn new(ledger: Arc<Mutex<Ledger<C, F>>>, run_generation: u64) -> Self {
        Self {
            ledger,
            run_generation,
        }
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
        let (kind, payload) = journal_record(entry)?;
        self.ledger
            .lock()
            .map_err(|_| TurnError::Journal("shared ledger lock is poisoned".to_owned()))?
            .append_required_payload(kind, &payload, self.run_generation)
            .map_err(|error| TurnError::Journal(error.to_string()))
    }
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
            } => (
                AuditKind::ThreadBound,
                Ok(
                    serde_json::json!({"operation_id":operation_id,"thread_id":thread_id,"thread_generation":thread_generation}),
                ),
            ),
            JournalEntry::TurnAccepted {
                operation_id,
                thread_id,
                turn_id,
            } => (
                AuditKind::TurnStarted,
                Ok(
                    serde_json::json!({"operation_id":operation_id,"thread_id":thread_id,"turn_id":turn_id}),
                ),
            ),
            JournalEntry::InteractionOpened { request_id, method } => (
                AuditKind::InteractionOpened,
                Ok(serde_json::json!({"request_id":request_id.to_string(),"method":method})),
            ),
            JournalEntry::InteractionResolved { request_id } => (
                AuditKind::InteractionResolved,
                Ok(serde_json::json!({"request_id":request_id.to_string()})),
            ),
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
/// bounded profile diagnostic metadata."  specs.md closes the v1 Run
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

pub struct TurnCoordinator<S, J, A> {
    server: S,
    journal: J,
    artifacts: A,
    state: CoordinatorState,
    attach: ThreadAttach,
    thread_id: Option<String>,
    thread_generation: u64,
    active_turn_id: Option<String>,
    /// The effort the active Turn was started with, so its terminal reports
    /// what it actually ran at rather than the Run's default.
    active_turn_effort: Option<String>,
    active_operation_id: Option<Uuid>,
    terminal_turn_ids: BTreeSet<String>,
    pending: BTreeMap<u64, Interaction>,
    completed_item_count: usize,
    ignored_foreign: Vec<IgnoredForeignRequest>,
    /// Foreign-thread requests this Run dropped without a durable diagnostic:
    /// past the recording bound, or because the profile journal refused the
    /// write.  Counted rather than silently lost, and never this Run's fault.
    undiagnosed_foreign: u64,
    streamed_history_bytes: u64,
    idempotency: BTreeMap<String, IdempotencyRecord>,
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
    foreign_diagnostics: Box<dyn ForeignDiagnostics>,
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
            "capabilities": {"experimentalApi":false,"optOutNotificationMethods":[]}
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
            active_turn_id: None,
            active_turn_effort: None,
            active_operation_id: None,
            terminal_turn_ids: BTreeSet::new(),
            pending: BTreeMap::new(),
            completed_item_count: 0,
            ignored_foreign: Vec::new(),
            undiagnosed_foreign: 0,
            streamed_history_bytes: 0,
            idempotency: BTreeMap::new(),
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
            foreign_diagnostics: config.foreign_diagnostics,
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
    /// The reasoning effort the active Turn was started with.
    #[must_use]
    pub fn active_turn_effort(&self) -> Option<&str> {
        self.active_turn_effort.as_deref()
    }
    #[must_use]
    pub fn pending_interactions(&self) -> Vec<&Interaction> {
        self.pending.values().collect()
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

    pub fn start_turn(&mut self, request: TurnRequest) -> Result<AcceptedTurn, TurnError> {
        if self.state == CoordinatorState::OutcomeUnknown {
            return Err(TurnError::OutcomeUnknown);
        }
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
        let digest = request_digest(&request, &effort)?;
        if let Some(record) = self.idempotency.get(&request.idempotency_key) {
            if record.digest != digest {
                return Err(TurnError::IdempotencyConflict {
                    recorded: record.digest.clone(),
                    observed: digest.clone(),
                });
            }
            let mut accepted = record.accepted.clone();
            accepted.replayed = true;
            return Ok(accepted);
        }
        if matches!(
            self.state,
            CoordinatorState::Running | CoordinatorState::WaitingInteraction
        ) {
            return Err(TurnError::TurnBusy);
        }
        let operation_id = Uuid::now_v7();
        let intent = OperationIntent {
            operation_id,
            idempotency_key: request.idempotency_key.clone(),
            request_sha256: digest.clone(),
            run_generation: self.run_generation,
            server_key: self.server_key.clone(),
            server_epoch: self.server_epoch,
            provisional_thread: self.thread_id.is_none(),
        };
        self.journal.append_and_sync(JournalEntry::Intent(intent))?;
        let thread_id = match self.ensure_thread(operation_id) {
            Ok(thread) => thread,
            Err(error) => return Err(self.lost_after_write(operation_id, error)),
        };
        let input = turn_input(&request)?;
        let result = match self.server.request("turn/start", serde_json::json!({
            "threadId": thread_id, "input": input, "model": self.fixed_model, "effort": effort,
            "sandboxPolicy": turn_sandbox(&self.sandbox, &self.cwd), "approvalPolicy": self.approval_policy
        })) {
            Ok(result) => result,
            Err(error) => return Err(self.lost_after_write(operation_id, error)),
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
            self.journal.append_and_sync(JournalEntry::ThreadBound {
                operation_id,
                thread_id: thread_id.clone(),
                thread_generation: self.thread_generation,
            })?;
        }
        self.journal.append_and_sync(JournalEntry::TurnAccepted {
            operation_id,
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
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

    /// Start a Turn and return as soon as the app-server has accepted it, or
    /// return the terminal a replayed idempotency key already earned.
    ///
    /// This is the half of `deliver` that never waits.  A worker that drains
    /// the app-server on its own thread accepts here and reports the outcome
    /// from the drain, so no caller has to hold the Run still while a Turn runs.
    pub fn accept(&mut self, request: TurnRequest) -> Result<DeliveryResult, TurnError> {
        let accepted = self.start_turn(request)?;
        if accepted.replayed
            && let Some(terminal) = self
                .idempotency
                .values()
                .find(|record| record.accepted.turn_id == accepted.turn_id)
                .and_then(|record| record.terminal.clone())
        {
            return Ok(DeliveryResult::Terminal(terminal));
        }
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
    /// specs.md: "`TRANSPORT_FAILURE` is retryable only when the operation
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
        let supported = matches!(
            method,
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/tool/requestUserInput"
        );
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
        let interaction = Interaction {
            request_id,
            method: method.to_owned(),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            payload_sha256: sha256_hex(&raw),
            byte_length: raw.len(),
        };
        self.journal
            .append_and_sync(JournalEntry::InteractionOpened {
                request_id,
                method: method.to_owned(),
            })?;
        self.pending.insert(request_id, interaction);
        self.state = CoordinatorState::WaitingInteraction;
        Ok(())
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
    ///   handed another Thread's decision. specs.md closes the v1 Run
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

    pub fn respond(&mut self, request_id: u64, response: Value) -> Result<(), TurnError> {
        if !self.pending.contains_key(&request_id) {
            return Err(TurnError::InteractionNotFound { request_id });
        }
        let raw = serde_json::to_vec(&response)
            .map_err(|_| TurnError::InvalidInput("interaction response is invalid"))?;
        if raw.len() > MAX_INTERACTION_PAYLOAD_BYTES {
            return Err(TurnError::InvalidInput("interaction response is too large"));
        }
        self.journal
            .append_and_sync(JournalEntry::InteractionResolved { request_id })?;
        if let Err(error) = self.server.respond_result(request_id, response) {
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
        Ok(())
    }

    /// Ask the app-server to interrupt the live Turn.
    ///
    /// `turn/interrupt` is an external write like any other, so losing its
    /// answer leaves the Turn's outcome uncertain rather than merely unsent:
    /// the app-server may have interrupted it, may have let it run on, and
    /// this Run can no longer tell.  specs.md forbids reporting that as a
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

fn request_digest(request: &TurnRequest, effort: &str) -> Result<String, TurnError> {
    let value = serde_json::json!({
        "message": request.message,
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

fn turn_sandbox(sandbox: &str, cwd: &Path) -> Value {
    if sandbox == "read-only" {
        // SPEC-002 pins the reader's turn policy whole: a `readOnly` sandbox
        // that omits `networkAccess` leaves the one property that separates a
        // reader from an unconstrained turn to the server's default.
        serde_json::json!({"type":"readOnly","networkAccess":false})
    } else {
        serde_json::json!({"type":"workspaceWrite","writableRoots":[cwd],"networkAccess":false,"excludeSlashTmp":true,"excludeTmpdirEnvVar":true})
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
    use std::collections::VecDeque;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    #[derive(Default)]
    struct FakeServer {
        replies: VecDeque<Result<Value, TurnError>>,
        messages: VecDeque<Value>,
        calls: Vec<(String, Value)>,
        responses: Vec<(u64, Value)>,
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
        mut server: FakeServer,
        attach: ThreadAttach,
        foreign_diagnostics: Box<dyn ForeignDiagnostics>,
        safety_policy: SessionSafetyPolicy,
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
            },
            "/tmp/codex-home",
        )
        .unwrap()
    }

    fn request(key: &str, delivery: DeliveryMode) -> TurnRequest {
        TurnRequest {
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
        server.messages.push_back(serde_json::json!({"id":9,"method":"item/tool/requestUserInput","params":{"threadId":"thread-1","turnId":"turn-1","questions":[{"id":"q"}]}}));
        let mut coordinator = coordinator(server);
        coordinator
            .start_turn(request("key", DeliveryMode::Submit))
            .unwrap();
        assert!(coordinator.next_event().unwrap().is_none());
        assert_eq!(coordinator.state(), &CoordinatorState::WaitingInteraction);
        assert_eq!(coordinator.pending_interactions()[0].request_id, 9);
        coordinator
            .respond(9, serde_json::json!({"answers":{"q":"secret"}}))
            .unwrap();
        assert_eq!(coordinator.state(), &CoordinatorState::Running);
        assert!(
            coordinator
                .journal()
                .entries
                .iter()
                .all(|entry| !format!("{entry:?}").contains("secret"))
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
        // specs.md: "`TRANSPORT_FAILURE` is retryable only when the operation
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
                }))
                .unwrap();
        }
        let ledger = Ledger::open(&root, run_id).unwrap();
        assert_eq!(ledger.durable_records().unwrap().len(), 1);
        assert_eq!(
            ledger.durable_records().unwrap()[0].kind(),
            AuditKind::IdempotencyReserved
        );
        drop(ledger);
        std::fs::remove_dir_all(root).unwrap();
    }
}
