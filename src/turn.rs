//! Thread and Turn lifecycle owned by one Run worker connection.

use crate::app_server::{JsonRpcConnection, TransportError, Wire};
use crate::audit::AuditKind;
use crate::fault::FaultInjector;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::ledger::{Ledger, LedgerClock};
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
use uuid::Uuid;

pub const MAX_INLINE_FINAL_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_FINAL_RESPONSE_ARTIFACT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_INTERACTION_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TURN_ITEMS: usize = 16_384;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnError {
    InvalidInput(&'static str),
    ModelMismatch,
    EffortUnsupported,
    TurnBusy,
    IdempotencyConflict,
    Transport(String),
    CorrelationMismatch,
    DuplicateTerminal,
    OutcomeUnknown,
    InteractionNotFound,
    Journal(String),
    Artifact(String),
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(formatter, "invalid turn input: {reason}"),
            Self::ModelMismatch => formatter.write_str("turn model differs from fixed model"),
            Self::EffortUnsupported => formatter.write_str("reasoning effort is not advertised"),
            Self::TurnBusy => formatter.write_str("run already has an active turn"),
            Self::IdempotencyConflict => {
                formatter.write_str("idempotency key was reused with different input")
            }
            Self::Transport(reason) => write!(formatter, "app-server transport failed: {reason}"),
            Self::CorrelationMismatch => {
                formatter.write_str("thread or turn correlation failed closed")
            }
            Self::DuplicateTerminal => {
                formatter.write_str("duplicate terminal event failed closed")
            }
            Self::OutcomeUnknown => formatter.write_str("turn acceptance or outcome is unknown"),
            Self::InteractionNotFound => formatter.write_str("interaction is not pending"),
            Self::Journal(reason) => write!(formatter, "durable journal failed: {reason}"),
            Self::Artifact(reason) => write!(formatter, "response artifact failed: {reason}"),
        }
    }
}

impl std::error::Error for TurnError {}

impl TurnError {
    #[must_use]
    pub fn into_machine_error(self) -> MachineError {
        let message = self.to_string();
        match self {
            Self::InvalidInput(_) => MachineError::invalid_argument("turn", message),
            Self::ModelMismatch | Self::EffortUnsupported => MachineError::new(
                "COMPATIBILITY_REJECTED",
                message,
                false,
                serde_json::json!({}),
            ),
            Self::TurnBusy => MachineError::new("RUN_BUSY", message, true, serde_json::json!({})),
            Self::IdempotencyConflict => MachineError::new(
                "IDEMPOTENCY_CONFLICT",
                message,
                false,
                serde_json::json!({}),
            ),
            Self::Transport(_) => {
                MachineError::new("TRANSPORT_FAILURE", message, true, serde_json::json!({}))
            }
            Self::CorrelationMismatch | Self::DuplicateTerminal | Self::OutcomeUnknown => {
                MachineError::new("OUTCOME_UNKNOWN", message, false, serde_json::json!({}))
            }
            Self::InteractionNotFound => MachineError::new(
                "INTERACTION_NOT_FOUND",
                message,
                false,
                serde_json::json!({}),
            ),
            Self::Journal(_) => {
                MachineError::new("INTERNAL_ERROR", message, false, serde_json::json!({}))
            }
            Self::Artifact(_) => MachineError::new(
                "ARTIFACT_INTEGRITY_FAILURE",
                message,
                false,
                serde_json::json!({}),
            ),
        }
    }
}

impl From<TransportError> for TurnError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value.to_string())
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

#[derive(Clone, Debug, Eq, PartialEq)]
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
    Terminal {
        turn_id: String,
        status: String,
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
        let (kind, payload) = match entry {
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
            JournalEntry::Terminal { turn_id, status } => (
                AuditKind::TurnTerminal,
                Ok(serde_json::json!({"turn_id":turn_id,"status":status})),
            ),
            JournalEntry::OutcomeUnknown { operation_id } => (
                AuditKind::OutcomeUnknown,
                Ok(serde_json::json!({"operation_id":operation_id})),
            ),
        };
        let payload = payload.map_err(|error| TurnError::Journal(error.to_string()))?;
        self.ledger
            .append_required_payload(kind, &payload, self.run_generation)
            .map_err(|error| TurnError::Journal(error.to_string()))
    }
}

pub trait AppServer {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, TurnError>;
    fn notify(&mut self, method: &str, params: Value) -> Result<(), TurnError>;
    fn next_message(&mut self) -> Result<Value, TurnError>;
    fn respond_result(&mut self, id: u64, result: Value) -> Result<(), TurnError>;
    fn respond_error(&mut self, id: u64, code: i64, message: &str) -> Result<(), TurnError>;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedTurn {
    pub thread_id: String,
    pub turn_id: String,
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

#[derive(Clone, Debug, Eq, PartialEq)]
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
        artifact_id: String,
        byte_length: u64,
        sha256: String,
    },
    Unavailable {
        byte_length: u64,
        sha256: String,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTurn {
    pub thread_id: String,
    pub turn_id: String,
    pub status: String,
    pub final_response: Option<FinalResponse>,
    pub usage: Option<Value>,
}

pub trait ResponseArtifactStore {
    fn store(&mut self, bytes: &[u8]) -> Result<String, TurnError>;
}

#[derive(Default)]
pub struct MemoryArtifactStore {
    pub values: BTreeMap<String, Vec<u8>>,
}
impl ResponseArtifactStore for MemoryArtifactStore {
    fn store(&mut self, bytes: &[u8]) -> Result<String, TurnError> {
        let id = format!("response-{}", self.values.len() + 1);
        self.values.insert(id.clone(), bytes.to_vec());
        Ok(id)
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
    active_operation_id: Option<Uuid>,
    terminal_turn_ids: BTreeSet<String>,
    pending: BTreeMap<u64, Interaction>,
    completed_item_count: usize,
    idempotency: BTreeMap<String, IdempotencyRecord>,
    fixed_model: String,
    default_effort: String,
    supported_efforts: BTreeSet<String>,
    cwd: PathBuf,
    developer_instructions: String,
    sandbox: String,
    approval_policy: String,
    run_generation: u64,
    server_key: String,
    server_epoch: u64,
}

pub struct CoordinatorConfig {
    pub attach: ThreadAttach,
    pub fixed_model: String,
    pub default_effort: String,
    pub supported_efforts: BTreeSet<String>,
    pub cwd: PathBuf,
    pub developer_instructions: String,
    pub sandbox: String,
    pub approval_policy: String,
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
            return Err(TurnError::EffortUnsupported);
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
            active_operation_id: None,
            terminal_turn_ids: BTreeSet::new(),
            pending: BTreeMap::new(),
            completed_item_count: 0,
            idempotency: BTreeMap::new(),
            fixed_model: config.fixed_model,
            default_effort: config.default_effort,
            supported_efforts: config.supported_efforts,
            cwd: config.cwd,
            developer_instructions: config.developer_instructions,
            sandbox: config.sandbox,
            approval_policy: config.approval_policy,
            run_generation: config.run_generation,
            server_key: config.server_key,
            server_epoch: config.server_epoch,
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
    #[must_use]
    pub fn active_turn_id(&self) -> Option<&str> {
        self.active_turn_id.as_deref()
    }
    #[must_use]
    pub fn pending_interactions(&self) -> Vec<&Interaction> {
        self.pending.values().collect()
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
            return Err(TurnError::ModelMismatch);
        }
        let effort = request
            .effort
            .clone()
            .unwrap_or_else(|| self.default_effort.clone());
        if !self.supported_efforts.contains(&effort) {
            return Err(TurnError::EffortUnsupported);
        }
        for image in &request.images {
            image.verify()?;
        }
        let digest = request_digest(&request, &effort)?;
        if let Some(record) = self.idempotency.get(&request.idempotency_key) {
            if record.digest != digest {
                return Err(TurnError::IdempotencyConflict);
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
        let (thread_id, newly_bound) = match self.ensure_thread(operation_id) {
            Ok(thread) => thread,
            Err(error) => {
                self.mark_unknown(operation_id)?;
                return Err(error);
            }
        };
        let input = turn_input(&request)?;
        let result = match self.server.request("turn/start", serde_json::json!({
            "threadId": thread_id, "input": input, "model": self.fixed_model, "effort": effort,
            "sandboxPolicy": turn_sandbox(&self.sandbox, &self.cwd), "approvalPolicy": self.approval_policy
        })) {
            Ok(result) => result,
            Err(error) => { self.mark_unknown(operation_id)?; return Err(error); }
        };
        let Some(turn_id) =
            nested_identity(&result, "turn", "id").or_else(|| identity(&result, "turnId"))
        else {
            self.mark_unknown(operation_id)?;
            return Err(TurnError::CorrelationMismatch);
        };
        if newly_bound {
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
        self.active_operation_id = Some(operation_id);
        self.state = CoordinatorState::Running;
        let accepted = AcceptedTurn {
            thread_id,
            turn_id,
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

    pub fn deliver(&mut self, request: TurnRequest) -> Result<DeliveryResult, TurnError> {
        let mode = request.delivery;
        let accepted = self.start_turn(request)?;
        if accepted.replayed {
            if let Some(terminal) = self
                .idempotency
                .values()
                .find(|record| record.accepted.turn_id == accepted.turn_id)
                .and_then(|record| record.terminal.clone())
            {
                return Ok(DeliveryResult::Terminal(terminal));
            }
            return Ok(DeliveryResult::Accepted(accepted));
        }
        if mode == DeliveryMode::Submit {
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

    fn ensure_thread(&mut self, operation_id: Uuid) -> Result<(String, bool), TurnError> {
        if let Some(thread_id) = &self.thread_id {
            return Ok((thread_id.clone(), false));
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
        Ok((thread_id, true))
    }

    fn mark_unknown(&mut self, operation_id: Uuid) -> Result<(), TurnError> {
        self.journal
            .append_and_sync(JournalEntry::OutcomeUnknown { operation_id })?;
        self.active_turn_id = None;
        self.active_operation_id = None;
        self.state = CoordinatorState::OutcomeUnknown;
        Ok(())
    }

    pub fn next_event(&mut self) -> Result<Option<TerminalTurn>, TurnError> {
        let message = match self.server.next_message() {
            Ok(message) => message,
            Err(error) => {
                if let Some(operation_id) = self.active_operation_id {
                    self.mark_unknown(operation_id)?;
                }
                return Err(error);
            }
        };
        let result = self.handle_message(message);
        if result.is_err()
            && let Some(operation_id) = self.active_operation_id
        {
            self.mark_unknown(operation_id)?;
        }
        result
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
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .ok_or(TurnError::CorrelationMismatch)?;
        let turn_id = event_turn_id(params).ok_or(TurnError::CorrelationMismatch)?;
        if self.thread_id.as_deref() != Some(thread_id)
            || self.active_turn_id.as_deref() != Some(turn_id)
        {
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

    pub fn respond(&mut self, request_id: u64, response: Value) -> Result<(), TurnError> {
        if !self.pending.contains_key(&request_id) {
            return Err(TurnError::InteractionNotFound);
        }
        let raw = serde_json::to_vec(&response)
            .map_err(|_| TurnError::InvalidInput("interaction response is invalid"))?;
        if raw.len() > MAX_INTERACTION_PAYLOAD_BYTES {
            return Err(TurnError::InvalidInput("interaction response is too large"));
        }
        self.journal
            .append_and_sync(JournalEntry::InteractionResolved { request_id })?;
        if let Err(error) = self.server.respond_result(request_id, response) {
            if let Some(operation_id) = self.active_operation_id {
                self.mark_unknown(operation_id)?;
            }
            return Err(error);
        }
        self.pending.remove(&request_id);
        self.state = if self.pending.is_empty() {
            CoordinatorState::Running
        } else {
            CoordinatorState::WaitingInteraction
        };
        Ok(())
    }

    pub fn interrupt(&mut self) -> Result<(), TurnError> {
        let thread_id = self.thread_id.clone().ok_or(TurnError::TurnBusy)?;
        let turn_id = self.active_turn_id.clone().ok_or(TurnError::TurnBusy)?;
        self.server.request(
            "turn/interrupt",
            serde_json::json!({"threadId":thread_id,"turnId":turn_id}),
        )?;
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
        let turn_id = event_turn_id(params).ok_or(TurnError::CorrelationMismatch)?;
        if self.thread_id.as_deref() != Some(thread_id) {
            return Ok(());
        }
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
        self.journal.append_and_sync(JournalEntry::Terminal {
            turn_id: turn_id.clone(),
            status: status.clone(),
        })?;
        self.terminal_turn_ids.insert(turn_id.clone());
        self.active_turn_id = None;
        self.active_operation_id = None;
        self.pending.clear();
        self.state = CoordinatorState::Idle;
        let authoritative_items = self.authoritative_items(&thread_id, &turn_id, turn)?;
        let final_response =
            self.extract_final_response(&thread_id, &turn_id, &authoritative_items)?;
        let terminal = TerminalTurn {
            thread_id,
            turn_id,
            status,
            final_response,
            usage: turn.get("usage").cloned(),
        };
        for record in self.idempotency.values_mut() {
            if record.accepted.turn_id == terminal.turn_id {
                record.terminal = Some(terminal.clone());
            }
        }
        Ok(terminal)
    }

    fn authoritative_items(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        terminal_turn: &Value,
    ) -> Result<Vec<Value>, TurnError> {
        if let Some(items) = terminal_turn.get("items").and_then(Value::as_array) {
            return Ok(items.clone());
        }
        let history = self.server.request(
            "thread/read",
            serde_json::json!({"threadId":thread_id,"includeTurns":true}),
        )?;
        let turns = history
            .get("thread")
            .and_then(|thread| thread.get("turns"))
            .or_else(|| history.get("turns"))
            .and_then(Value::as_array)
            .ok_or(TurnError::CorrelationMismatch)?;
        let turn = turns
            .iter()
            .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id))
            .ok_or(TurnError::CorrelationMismatch)?;
        turn.get("items")
            .and_then(Value::as_array)
            .cloned()
            .ok_or(TurnError::CorrelationMismatch)
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
                reason: "response_too_large".to_owned(),
            }));
        }
        match self.artifacts.store(bytes) {
            Ok(artifact_id) => Ok(Some(FinalResponse::Artifact {
                artifact_id,
                byte_length,
                sha256,
            })),
            Err(_) => Ok(Some(FinalResponse::Unavailable {
                byte_length,
                sha256,
                reason: "durable_write_failed".to_owned(),
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
        serde_json::json!({"type":"readOnly"})
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
            self.replies
                .pop_front()
                .unwrap_or(Err(TurnError::Transport("missing fake reply".to_owned())))
        }
        fn notify(&mut self, method: &str, params: Value) -> Result<(), TurnError> {
            self.calls.push((method.to_owned(), params));
            Ok(())
        }
        fn next_message(&mut self) -> Result<Value, TurnError> {
            self.messages
                .pop_front()
                .ok_or_else(|| TurnError::Transport("missing fake message".to_owned()))
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

    fn coordinator(
        server: FakeServer,
    ) -> TurnCoordinator<FakeServer, MemoryJournal, MemoryArtifactStore> {
        coordinator_with_attach(server, ThreadAttach::Start)
    }

    fn coordinator_with_attach(
        mut server: FakeServer,
        attach: ThreadAttach,
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
                fixed_model: "gpt-5".to_owned(),
                default_effort: "medium".to_owned(),
                supported_efforts: ["low".to_owned(), "medium".to_owned(), "high".to_owned()]
                    .into_iter()
                    .collect(),
                cwd: PathBuf::from("/tmp/workspace"),
                developer_instructions: "fixed".to_owned(),
                sandbox: "read-only".to_owned(),
                approval_policy: "untrusted".to_owned(),
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
        assert_eq!(
            coordinator.start_turn(changed).unwrap_err(),
            TurnError::IdempotencyConflict
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
        } = terminal.final_response.unwrap()
        else {
            panic!("large response must use an artifact");
        };
        let stored = &coordinator.artifacts().values[&artifact_id];
        assert_eq!(byte_length, stored.len() as u64);
        assert_eq!(sha256, sha256_hex(stored));
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
        assert_eq!(
            coordinator.start_turn(unsupported).unwrap_err(),
            TurnError::EffortUnsupported
        );
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
        server
            .replies
            .push_back(Err(TurnError::Transport("response lost".to_owned())));
        let mut coordinator = coordinator(server);
        assert!(matches!(
            coordinator.start_turn(request("key", DeliveryMode::Submit)),
            Err(TurnError::Transport(_))
        ));
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
