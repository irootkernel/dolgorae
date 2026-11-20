use crate::audit::{AuditError, AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH};
use crate::domain::RunLifecycle;
use crate::fault::{FaultInjector, NoFaults};
use crate::jcs::{LosslessJson, canonicalize, parse};
use crate::ledger::{
    AppendDurability, Ledger, LedgerClock, LedgerError, MAX_AUDIT_LINE_BYTES, MAX_STATE_BYTES,
    RunStateProjection, SystemLedgerClock,
};
use crate::workspace::{
    SystemWorkspacePlatform, WorkspacePlatform, sync_directory, verify_secure_directory,
    verify_secure_file,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const BOOTSTRAP_IDEMPOTENCY_RECORD_INDEX: usize = 1;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyOperation {
    StartRun,
    ForkRun,
    SubmitTurn,
    ResolveInteraction,
    CreateWriteContinuation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyIntent {
    pub schema_version: u32,
    pub operation: IdempotencyOperation,
    pub idempotency_key: String,
    pub normalized_identity_sha256: String,
    pub run_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeOperationIntent {
    operation_id: Uuid,
    idempotency_key: String,
    request_sha256: String,
    run_generation: u64,
    server_key: String,
    server_epoch: u64,
    #[serde(rename = "provisional_thread")]
    _provisional_thread: bool,
    #[serde(default, rename = "accepted_input")]
    accepted_input: Option<RuntimeAcceptedUserInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeAcceptedUserInput {
    content: RuntimeAcceptedInputContent,
    images: Vec<RuntimeAcceptedImageMetadata>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RuntimeAcceptedInputContent {
    Inline {
        storage: RuntimeAcceptedInputStorage,
    },
    Artifact {
        storage: RuntimeAcceptedInputStorage,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeAcceptedInputStorage {
    artifact_id: Uuid,
    created_at: String,
    byte_length: u64,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeAcceptedImageMetadata {
    ordinal: u32,
    detail: RuntimeAcceptedImageDetail,
    media_type: String,
    byte_length: u64,
    sha256: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeAcceptedImageDetail {
    Auto,
    Low,
    High,
}

impl RuntimeAcceptedInputContent {
    const fn storage(&self) -> &RuntimeAcceptedInputStorage {
        match self {
            Self::Inline { storage } | Self::Artifact { storage } => storage,
        }
    }
}

impl IdempotencyIntent {
    pub fn validate(&self) -> Result<(), ConformanceError> {
        if self.schema_version != 1
            || !utf8_bounded(&self.idempotency_key, 256)
            || !is_sha256(&self.normalized_identity_sha256)
            || self.run_id.get_version_num() != 7
        {
            return Err(ConformanceError::InvalidIntent(
                "idempotency intent violates the checked v1 shape".to_owned(),
            ));
        }
        Ok(())
    }

    fn as_payload(&self) -> Result<LosslessJson, ConformanceError> {
        self.validate()?;
        let serialized = serde_json::to_string(self)
            .map_err(|error| ConformanceError::InvalidIntent(error.to_string()))?;
        parse(&serialized).map_err(|error| ConformanceError::InvalidIntent(error.to_string()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapRecordKind {
    RunCreated,
    WriteContinuationCreated,
}

impl BootstrapRecordKind {
    const fn audit_kind(self) -> AuditKind {
        match self {
            Self::RunCreated => AuditKind::RunCreated,
            Self::WriteContinuationCreated => AuditKind::WriteContinuationCreated,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrapRequest {
    pub timestamp: String,
    pub workspace_id: String,
    pub intent: IdempotencyIntent,
    pub record_kind: BootstrapRecordKind,
    pub initial_access: String,
    pub default_effort: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "TASK-004 supplies the generation probe verdict")]
pub(crate) enum GenerationVerdict {
    Absent,
    Present,
    Unverifiable,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StartFailureAuthority {
    run_id: Uuid,
    _verified_bootstrap_election: (),
}

impl StartFailureAuthority {
    /// The caller owns the startup lock and has proved the exact generation retired.
    pub(crate) fn from_retired_startup_abort(run_id: Uuid) -> Self {
        Self {
            run_id,
            _verified_bootstrap_election: (),
        }
    }

    /// This constructor is intentionally crate-private. The startup/recovery
    /// owner may mint the capability only after its byte-0 lock and absence
    /// probes have succeeded; API consumers cannot assert those facts with
    /// booleans or deserialize a capability.
    #[allow(dead_code, reason = "TASK-004 startup owner will mint this capability")]
    pub(crate) fn from_verified_bootstrap_election(
        run_id: Uuid,
        prior_generation: GenerationVerdict,
        worker_reached_bound: bool,
        profile_server_ready: bool,
    ) -> Result<Self, ConformanceError> {
        if run_id.get_version_num() != 7
            || prior_generation != GenerationVerdict::Absent
            || worker_reached_bound
            || !profile_server_ready
        {
            return Err(ConformanceError::UnauthorizedStartFailure);
        }
        Ok(Self {
            run_id,
            _verified_bootstrap_election: (),
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ConfirmedInterruptAuthority {
    run_id: Uuid,
    previous: RunLifecycle,
    _verified_terminal_observation: (),
}

impl ConfirmedInterruptAuthority {
    #[allow(
        dead_code,
        reason = "TASK-004 interrupt owner will mint this capability"
    )]
    pub(crate) fn from_verified_terminal_observation(
        run_id: Uuid,
        previous: RunLifecycle,
    ) -> Result<Self, ConformanceError> {
        if run_id.get_version_num() != 7
            || !matches!(
                previous,
                RunLifecycle::Running | RunLifecycle::WaitingInteraction
            )
        {
            return Err(ConformanceError::InvalidHistory(
                "interrupt authority requires a running or waiting Run".to_owned(),
            ));
        }
        Ok(Self {
            run_id,
            previous,
            _verified_terminal_observation: (),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceReport {
    pub run_id: Uuid,
    pub record_count: u64,
    pub lifecycle: RunLifecycle,
    pub terminal_sealed: bool,
    pub bootstrap_kind: Option<BootstrapRecordKind>,
    pub idempotency_intent: Option<IdempotencyIntent>,
    pub head_hash: String,
}

#[derive(Debug)]
pub enum ConformanceError {
    Ledger(LedgerError),
    Io(std::io::Error),
    InvalidIntent(String),
    InvalidHistory(String),
    ReservationConflict,
    ReservationPending,
    ReservationLost,
    UnauthorizedStartFailure,
    ConfirmationRequired,
    DeleteNotTerminal,
    Security(String),
}

impl std::fmt::Display for ConformanceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
            Self::InvalidIntent(reason) => {
                write!(formatter, "invalid idempotency intent: {reason}")
            }
            Self::InvalidHistory(reason) => {
                write!(formatter, "ledger conformance failure: {reason}")
            }
            Self::ReservationConflict => {
                formatter.write_str("idempotency identity conflicts with the recorded key")
            }
            Self::ReservationPending => {
                formatter.write_str("idempotency key has an unaccepted reservation")
            }
            Self::ReservationLost => {
                formatter.write_str("idempotency reservation is no longer owned by this guard")
            }
            Self::UnauthorizedStartFailure => {
                formatter.write_str("bootstrap writer is not authorized to seal start_failed")
            }
            Self::ConfirmationRequired => formatter.write_str("confirmed deletion is required"),
            Self::DeleteNotTerminal => {
                formatter.write_str("only a closed or start_failed run may be deleted")
            }
            Self::Security(reason) => write!(formatter, "secure run deletion refused: {reason}"),
        }
    }
}

impl std::error::Error for ConformanceError {}

impl From<LedgerError> for ConformanceError {
    fn from(value: LedgerError) -> Self {
        Self::Ledger(value)
    }
}

impl From<AuditError> for ConformanceError {
    fn from(value: AuditError) -> Self {
        Self::Ledger(LedgerError::Audit(value))
    }
}

impl From<std::io::Error> for ConformanceError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone)]
pub struct IdempotencyReservations {
    inner: Arc<Mutex<BTreeMap<(IdempotencyOperation, String), ReservationState>>>,
}

#[derive(Clone, Debug)]
enum ReservationState {
    Pending {
        token: Uuid,
        identity: String,
        run_id: Uuid,
    },
    Accepted {
        identity: String,
        run_id: Uuid,
    },
}

pub enum ReservationResult {
    Acquired(IdempotencyReservation),
    ExactReplay(Uuid),
}

pub struct IdempotencyReservation {
    owner: IdempotencyReservations,
    operation: IdempotencyOperation,
    key: String,
    token: Uuid,
    accepted: bool,
}

impl Default for IdempotencyReservations {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl IdempotencyReservations {
    pub fn reserve(
        &self,
        intent: &IdempotencyIntent,
    ) -> Result<ReservationResult, ConformanceError> {
        intent.validate()?;
        let mut entries = self
            .inner
            .lock()
            .map_err(|_| ConformanceError::ReservationLost)?;
        let map_key = (intent.operation, intent.idempotency_key.clone());
        match entries.get(&map_key) {
            Some(ReservationState::Accepted { identity, run_id }) => {
                if identity == &intent.normalized_identity_sha256 && run_id == &intent.run_id {
                    Ok(ReservationResult::ExactReplay(*run_id))
                } else {
                    Err(ConformanceError::ReservationConflict)
                }
            }
            Some(ReservationState::Pending {
                identity, run_id, ..
            }) => {
                if identity == &intent.normalized_identity_sha256 && run_id == &intent.run_id {
                    Err(ConformanceError::ReservationPending)
                } else {
                    Err(ConformanceError::ReservationConflict)
                }
            }
            None => {
                let token = Uuid::now_v7();
                entries.insert(
                    map_key,
                    ReservationState::Pending {
                        token,
                        identity: intent.normalized_identity_sha256.clone(),
                        run_id: intent.run_id,
                    },
                );
                Ok(ReservationResult::Acquired(IdempotencyReservation {
                    owner: self.clone(),
                    operation: intent.operation,
                    key: intent.idempotency_key.clone(),
                    token,
                    accepted: false,
                }))
            }
        }
    }

    pub fn remember_accepted(&self, intent: &IdempotencyIntent) -> Result<(), ConformanceError> {
        intent.validate()?;
        let mut entries = self
            .inner
            .lock()
            .map_err(|_| ConformanceError::ReservationLost)?;
        let map_key = (intent.operation, intent.idempotency_key.clone());
        if let Some(existing) = entries.get(&map_key) {
            match existing {
                ReservationState::Pending { .. } => {
                    return Err(ConformanceError::ReservationPending);
                }
                ReservationState::Accepted { identity, run_id }
                    if identity != &intent.normalized_identity_sha256
                        || run_id != &intent.run_id =>
                {
                    return Err(ConformanceError::ReservationConflict);
                }
                ReservationState::Accepted { .. } => return Ok(()),
            }
        }
        entries.insert(
            map_key,
            ReservationState::Accepted {
                identity: intent.normalized_identity_sha256.clone(),
                run_id: intent.run_id,
            },
        );
        Ok(())
    }

    #[must_use]
    pub fn contains(&self, operation: IdempotencyOperation, key: &str) -> bool {
        self.inner
            .lock()
            .map(|entries| entries.contains_key(&(operation, key.to_owned())))
            .unwrap_or(true)
    }
}

impl IdempotencyReservation {
    pub fn accept_after_publication(
        mut self,
        report: &ConformanceReport,
    ) -> Result<Uuid, ConformanceError> {
        let accepted = report
            .idempotency_intent
            .as_ref()
            .ok_or(ConformanceError::ReservationLost)?;
        if accepted.operation != self.operation
            || accepted.idempotency_key != self.key
            || accepted.run_id != report.run_id
        {
            return Err(ConformanceError::ReservationLost);
        }
        let mut entries = self
            .owner
            .inner
            .lock()
            .map_err(|_| ConformanceError::ReservationLost)?;
        let map_key = (self.operation, self.key.clone());
        let Some(ReservationState::Pending {
            token,
            identity,
            run_id,
        }) = entries.get(&map_key).cloned()
        else {
            return Err(ConformanceError::ReservationLost);
        };
        if token != self.token {
            return Err(ConformanceError::ReservationLost);
        }
        if identity != accepted.normalized_identity_sha256 || run_id != accepted.run_id {
            return Err(ConformanceError::ReservationConflict);
        }
        entries.insert(map_key, ReservationState::Accepted { identity, run_id });
        self.accepted = true;
        Ok(run_id)
    }
}

impl Drop for IdempotencyReservation {
    fn drop(&mut self) {
        if self.accepted {
            return;
        }
        let Ok(mut entries) = self.owner.inner.lock() else {
            return;
        };
        let map_key = (self.operation, self.key.clone());
        if matches!(entries.get(&map_key), Some(ReservationState::Pending { token, .. }) if *token == self.token)
        {
            entries.remove(&map_key);
        }
    }
}

pub struct ConformantLedger<C: LedgerClock = SystemLedgerClock, F: FaultInjector = NoFaults> {
    ledger: Ledger<C, F>,
    bootstrap_recovery: bool,
    conformance: Option<ConformanceState>,
    tail_conformance: Option<ConformanceState>,
}

#[derive(Clone)]
struct ConformanceState {
    report: ConformanceReport,
    previous_generation: u64,
    previous_kind: Option<AuditKind>,
    active_turn: Option<String>,
    pending: BTreeSet<String>,
    runtime_intents: BTreeMap<String, String>,
    caller_native_intent_seen: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallerNativeIntent {
    pub schema: String,
    pub operation_id: Uuid,
    pub method: String,
}

impl CallerNativeIntent {
    pub(crate) fn validate(&self) -> Result<(), ConformanceError> {
        if self.schema != "dolgorae.caller-native-intent/v1"
            || self.operation_id.get_version_num() != 7
            || !matches!(
                self.method.as_str(),
                "thread/start" | "thread/settings/update"
            )
        {
            return Err(ConformanceError::InvalidHistory(
                "caller native intent is invalid".to_owned(),
            ));
        }
        Ok(())
    }
}

fn caller_native_intent(record: &AuditRecord) -> Result<bool, ConformanceError> {
    if record.kind() != AuditKind::TurnIntent {
        return Ok(false);
    }
    let bytes = canonicalize(record.payload())
        .map_err(|error| ConformanceError::InvalidHistory(error.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| ConformanceError::InvalidHistory(error.to_string()))?;
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some("dolgorae.caller-native-intent/v1")
    {
        return Ok(false);
    }
    let intent: CallerNativeIntent = serde_json::from_value(value).map_err(|_| {
        ConformanceError::InvalidHistory("caller native intent shape is invalid".to_owned())
    })?;
    intent.validate()?;
    Ok(true)
}

fn checked_outcome_unknown(
    previous: RunLifecycle,
    caller_native_intent_seen: bool,
) -> Result<RunLifecycle, ConformanceError> {
    if previous == RunLifecycle::Idle && caller_native_intent_seen {
        Ok(RunLifecycle::OutcomeUnknown)
    } else {
        checked_implicit_transition(previous, RunLifecycle::OutcomeUnknown)
    }
}

impl ConformantLedger<SystemLedgerClock, NoFaults> {
    pub fn open(root: impl Into<PathBuf>, run_id: Uuid) -> Result<Self, ConformanceError> {
        let ledger = Ledger::open(root, run_id)?;
        let conformance = Some(ConformanceState::from_verified(
            run_id,
            ledger.durable_records()?,
            || ledger.check_open_replay_budget().map_err(Into::into),
        )?);
        Ok(Self {
            ledger,
            bootstrap_recovery: false,
            tail_conformance: conformance.clone(),
            conformance,
        })
    }

    pub fn open_for_bootstrap(
        root: impl Into<PathBuf>,
        run_id: Uuid,
    ) -> Result<Self, ConformanceError> {
        let ledger = Ledger::open(root, run_id)?;
        if ledger.durable_records()?.len() > 3 {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap recovery found records beyond the closed prefix".to_owned(),
            ));
        }
        Ok(Self {
            ledger,
            bootstrap_recovery: true,
            conformance: None,
            tail_conformance: None,
        })
    }
}

impl<C: LedgerClock + 'static, F: FaultInjector + 'static> ConformantLedger<C, F> {
    pub fn open_with(
        root: impl Into<PathBuf>,
        run_id: Uuid,
        clock: C,
        faults: F,
    ) -> Result<Self, ConformanceError> {
        let ledger = Ledger::open_with(root, run_id, clock, faults)?;
        let conformance = Some(ConformanceState::from_verified(
            run_id,
            ledger.durable_records()?,
            || ledger.check_open_replay_budget().map_err(Into::into),
        )?);
        Ok(Self {
            ledger,
            bootstrap_recovery: false,
            tail_conformance: conformance.clone(),
            conformance,
        })
    }

    pub fn open_for_bootstrap_with(
        root: impl Into<PathBuf>,
        run_id: Uuid,
        clock: C,
        faults: F,
    ) -> Result<Self, ConformanceError> {
        let ledger = Ledger::open_with(root, run_id, clock, faults)?;
        if ledger.durable_records()?.len() > 3 {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap recovery found records beyond the closed prefix".to_owned(),
            ));
        }
        Ok(Self {
            ledger,
            bootstrap_recovery: true,
            conformance: None,
            tail_conformance: None,
        })
    }

    /// The caller holds Writer observation serialization through all required appends.
    pub fn with_event_context<T, E>(
        &mut self,
        context: crate::event::EventAppendContext,
        append: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let previous = self.ledger.replace_event_context(Some(context));
        let result = append(self);
        self.ledger.replace_event_context(previous);
        result
    }

    pub fn bootstrap(
        &mut self,
        request: &BootstrapRequest,
    ) -> Result<ConformanceReport, ConformanceError> {
        if request.intent.run_id != self.ledger.projection()?.run_id
            || !is_sha256(&request.workspace_id)
        {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap identity is inconsistent".to_owned(),
            ));
        }
        request.intent.validate()?;
        if !matches!(
            request.initial_access.as_str(),
            "read" | "write" | "unknown"
        ) || request
            .default_effort
            .as_deref()
            .is_some_and(|value| !utf8_bounded(value, 64))
        {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap projection fields are invalid".to_owned(),
            ));
        }
        let workspace = object(&[("workspace_id", string(&request.workspace_id))]);
        let mut created_fields = vec![("initial_access", string(&request.initial_access))];
        if let Some(effort) = &request.default_effort {
            created_fields.push(("default_effort", string(effort)));
        }
        let payloads = [
            (AuditKind::WorkspaceInitialized, workspace),
            (AuditKind::IdempotencyReserved, request.intent.as_payload()?),
            (request.record_kind.audit_kind(), object(&created_fields)),
        ];
        let mut previous = GENESIS_PREVIOUS_HASH.to_owned();
        let mut records = Vec::with_capacity(3);
        for (index, (kind, payload)) in payloads.into_iter().enumerate() {
            let record = AuditRecord::new(
                u64::try_from(index).unwrap_or(u64::MAX) + 1,
                &request.timestamp,
                request.intent.run_id,
                0,
                kind,
                payload,
                &previous,
            )?;
            let record = if let Some(existing) = self.ledger.durable_records()?.get(index) {
                match existing.client_projection() {
                    Some(projection) => record.with_client_projection(projection.clone())?,
                    None => record,
                }
            } else {
                self.ledger.prepare_event(record)?
            };
            previous = record.hash().to_owned();
            records.push(record);
        }
        let durable = self.ledger.durable_records()?;
        let mut mismatch = None;
        for (index, (existing, expected)) in durable.iter().zip(&records).enumerate() {
            if existing.canonical_line()? != expected.canonical_line()? {
                mismatch = Some(index);
                break;
            }
        }
        if durable.len() > records.len() || mismatch.is_some() {
            return Err(ConformanceError::InvalidHistory(format!(
                "bootstrap restart does not match the durable prefix at record {}",
                mismatch.map_or(records.len() + 1, |index| index + 1)
            )));
        }
        for record in records.into_iter().skip(durable.len()) {
            self.ledger.append(record, AppendDurability::Required)?;
        }
        let report = verify_ledger_records(request.intent.run_id, self.ledger.durable_records()?)?;
        self.bootstrap_recovery = false;
        self.conformance = Some(ConformanceState::from_verified(
            request.intent.run_id,
            self.ledger.durable_records()?,
            || Ok(()),
        )?);
        self.tail_conformance = self.conformance.clone();
        Ok(report)
    }

    pub fn append(
        &mut self,
        record: AuditRecord,
        durability: AppendDurability,
    ) -> Result<(), ConformanceError> {
        if self.bootstrap_recovery {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap recovery permits only exact bootstrap completion".to_owned(),
            ));
        }
        if matches!(
            record.kind(),
            AuditKind::StartFailed | AuditKind::CleanupResult
        ) || (record.kind() == AuditKind::LifecycleTransition
            && required_bool(record.payload(), "terminal_seal")?)
        {
            return Err(ConformanceError::InvalidHistory(
                "terminal evidence and seals require the authority-specific API".to_owned(),
            ));
        }
        self.ledger.flush()?;
        self.conformance = self.tail_conformance.clone();
        let mut candidate = self.tail_conformance.clone().ok_or_else(|| {
            ConformanceError::InvalidHistory("conformance state is not initialized".to_owned())
        })?;
        let record = self.ledger.prepare_event(record)?;
        candidate.apply(&record)?;
        candidate.require_complete_terminal_pair()?;
        if record.kind() == AuditKind::LifecycleTransition {
            self.ledger.append_conformance_record(record, durability)?;
        } else {
            self.ledger.append(record, durability)?;
        }
        self.tail_conformance = Some(candidate);
        if durability == AppendDurability::Required {
            self.conformance = self.tail_conformance.clone();
        }
        Ok(())
    }

    #[allow(
        dead_code,
        reason = "TASK-004 startup owner will call this sealed boundary"
    )]
    pub(crate) fn seal_start_failed(
        &mut self,
        timestamp: &str,
        reason: &str,
        authority: StartFailureAuthority,
    ) -> Result<ConformanceReport, ConformanceError> {
        if authority.run_id != self.ledger.projection()?.run_id {
            return Err(ConformanceError::UnauthorizedStartFailure);
        }
        let report = self.verify()?;
        if report.lifecycle != RunLifecycle::Starting
            || report.terminal_sealed
            || reason.is_empty()
            || !utf8_bounded(reason, 1024)
        {
            return Err(ConformanceError::InvalidHistory(
                "start_failed requires an unsealed starting run and bounded reason".to_owned(),
            ));
        }
        let evidence = self.next_record(
            timestamp,
            AuditKind::StartFailed,
            object(&[("reason", string(reason))]),
        )?;
        let seal = AuditRecord::new(
            evidence.sequence() + 1,
            timestamp,
            report.run_id,
            evidence.run_generation(),
            AuditKind::LifecycleTransition,
            transition_payload(RunLifecycle::Starting, RunLifecycle::StartFailed, true),
            evidence.hash(),
        )?;
        self.append_terminal_pair(evidence, seal)
    }

    pub(crate) fn verify_startup_abort_history(&self) -> Result<RunLifecycle, ConformanceError> {
        let report = self.verify()?;
        let projection = self.ledger.projection()?;
        if !matches!(
            report.lifecycle,
            RunLifecycle::Starting | RunLifecycle::StartFailed
        ) || projection.run_generation > 1
            || projection.thread_id.is_some()
            || projection.latest_turn_id.is_some()
        {
            return Err(ConformanceError::UnauthorizedStartFailure);
        }
        for record in self.ledger.durable_records()? {
            if caller_native_intent(record)? || record.kind() == AuditKind::TurnStarted {
                return Err(ConformanceError::UnauthorizedStartFailure);
            }
        }
        Ok(report.lifecycle)
    }

    pub fn seal_closed(
        &mut self,
        timestamp: &str,
        cleanup_outcome: &str,
    ) -> Result<ConformanceReport, ConformanceError> {
        let report = self.verify()?;
        if report.terminal_sealed || !utf8_bounded(cleanup_outcome, 1024) {
            return Err(ConformanceError::InvalidHistory(
                "close requires an unsealed run and bounded cleanup outcome".to_owned(),
            ));
        }
        if !matches!(
            report.lifecycle,
            RunLifecycle::Idle | RunLifecycle::Paused | RunLifecycle::OutcomeUnknown
        ) {
            return Err(ConformanceError::InvalidHistory(
                "current lifecycle cannot transition to closed".to_owned(),
            ));
        }
        let evidence = self.next_record(
            timestamp,
            AuditKind::CleanupResult,
            object(&[("outcome", string(cleanup_outcome))]),
        )?;
        let seal = AuditRecord::new(
            evidence.sequence() + 1,
            timestamp,
            report.run_id,
            evidence.run_generation(),
            AuditKind::LifecycleTransition,
            transition_payload(report.lifecycle, RunLifecycle::Closed, true),
            evidence.hash(),
        )?;
        self.append_terminal_pair(evidence, seal)
    }

    /// Pause an idle, unsealed Run without constructing a worker or touching
    /// its app-server lane. This is the authority used by a threadless Run:
    /// there is no external effect to interrupt or reconcile, but the
    /// lifecycle transition still belongs in the conformant durable ledger.
    pub fn pause_idle(&mut self, timestamp: &str) -> Result<ConformanceReport, ConformanceError> {
        let report = self.verify()?;
        if report.terminal_sealed || report.lifecycle != RunLifecycle::Idle {
            return Err(ConformanceError::InvalidHistory(
                "pause_idle requires an unsealed idle run".to_owned(),
            ));
        }
        let transition = self.next_record(
            timestamp,
            AuditKind::LifecycleTransition,
            transition_payload(RunLifecycle::Idle, RunLifecycle::Paused, false),
        )?;
        self.append(transition, AppendDurability::Required)?;
        self.verify()
    }

    /// Publish a newly allocated public Run as idle without constructing a
    /// worker or Codex thread. Lazy worker allocation must not leave a
    /// successfully returned Run in the transient `starting` lifecycle.
    pub fn mark_threadless_ready(
        &mut self,
        timestamp: &str,
    ) -> Result<ConformanceReport, ConformanceError> {
        let report = self.verify()?;
        if report.terminal_sealed || report.lifecycle != RunLifecycle::Starting {
            return Err(ConformanceError::InvalidHistory(
                "threadless readiness requires an unsealed starting run".to_owned(),
            ));
        }
        let transition = self.next_record(
            timestamp,
            AuditKind::LifecycleTransition,
            transition_payload(RunLifecycle::Starting, RunLifecycle::Idle, false),
        )?;
        self.append(transition, AppendDurability::Required)?;
        self.verify()
    }

    /// Append one idempotent Profile observation without changing Run
    /// lifecycle. Profile-wide operator interruption uses the existing closed
    /// audit-v1 `profile_observed` kind rather than inventing a new kind.
    pub(crate) fn append_profile_observation(
        &mut self,
        timestamp: &str,
        operation_id: Uuid,
        payload: &serde_json::Value,
    ) -> Result<bool, ConformanceError> {
        let operation_id_text = operation_id.to_string();
        let existing = self
            .ledger
            .payloads_of_kind(AuditKind::ProfileObserved)?
            .into_iter()
            .find(|value| {
                value
                    .get("operation_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(operation_id_text.as_str())
            });
        if let Some(existing) = existing {
            if existing == *payload {
                return Ok(false);
            }
            return Err(ConformanceError::InvalidHistory(
                "profile observation operation ID has conflicting durable input".to_owned(),
            ));
        }
        let encoded = serde_json::to_string(payload)
            .map_err(|error| ConformanceError::InvalidHistory(error.to_string()))?;
        let lossless =
            parse(&encoded).map_err(|error| ConformanceError::InvalidHistory(error.to_string()))?;
        let record = self.next_record(timestamp, AuditKind::ProfileObserved, lossless)?;
        self.append(record, AppendDurability::Required)?;
        Ok(true)
    }

    /// Conservatively terminate an active lifecycle whose worker is already
    /// absent and therefore cannot provide terminal evidence.
    pub(crate) fn mark_profile_interrupt_outcome_unknown(
        &mut self,
        timestamp: &str,
        operation_id: Uuid,
    ) -> Result<(), ConformanceError> {
        let report = self.verify()?;
        if report.lifecycle == RunLifecycle::OutcomeUnknown {
            return Ok(());
        }
        if !matches!(
            report.lifecycle,
            RunLifecycle::Running
                | RunLifecycle::WaitingInteraction
                | RunLifecycle::ReconciliationRequired
        ) {
            return Err(ConformanceError::InvalidHistory(
                "profile interrupt cannot classify this lifecycle as outcome_unknown".to_owned(),
            ));
        }
        let record = self.next_record(
            timestamp,
            AuditKind::OutcomeUnknown,
            object(&[("operation_id", string(&operation_id.to_string()))]),
        )?;
        self.append(record, AppendDurability::Required)
    }

    #[allow(
        dead_code,
        reason = "TASK-004 interrupt owner will call this sealed boundary"
    )]
    pub(crate) fn transition_after_confirmed_interrupt(
        &mut self,
        timestamp: &str,
        current: RunLifecycle,
        cleanup_outcome: Option<&str>,
        authority: ConfirmedInterruptAuthority,
    ) -> Result<ConformanceReport, ConformanceError> {
        let report = self.verify()?;
        if authority.run_id != report.run_id
            || authority.previous != report.lifecycle
            || report.terminal_sealed
            || !matches!(current, RunLifecycle::Paused | RunLifecycle::Closed)
        {
            return Err(ConformanceError::InvalidHistory(
                "confirmed interrupt authority does not match the requested transition".to_owned(),
            ));
        }
        if current == RunLifecycle::Paused {
            if cleanup_outcome.is_some() {
                return Err(ConformanceError::InvalidHistory(
                    "pause cannot carry close cleanup evidence".to_owned(),
                ));
            }
            let transition = self.next_record(
                timestamp,
                AuditKind::LifecycleTransition,
                interrupt_transition_payload(report.lifecycle, current, false),
            )?;
            self.append(transition, AppendDurability::Required)?;
            return self.verify();
        }
        let outcome = cleanup_outcome
            .filter(|value| utf8_bounded(value, 1024))
            .ok_or_else(|| {
                ConformanceError::InvalidHistory(
                    "interrupt close requires bounded cleanup evidence".to_owned(),
                )
            })?;
        let evidence = self.next_record(
            timestamp,
            AuditKind::CleanupResult,
            object(&[("outcome", string(outcome))]),
        )?;
        let seal = AuditRecord::new(
            evidence.sequence() + 1,
            timestamp,
            report.run_id,
            evidence.run_generation(),
            AuditKind::LifecycleTransition,
            interrupt_transition_payload(report.lifecycle, current, true),
            evidence.hash(),
        )?;
        self.append_terminal_pair(evidence, seal)
    }

    pub fn verify(&self) -> Result<ConformanceReport, ConformanceError> {
        let durable_count = u64::try_from(self.ledger.durable_records()?.len()).unwrap_or(u64::MAX);
        let state = self
            .tail_conformance
            .as_ref()
            .filter(|state| state.report.record_count == durable_count)
            .or(self.conformance.as_ref())
            .ok_or_else(|| {
                ConformanceError::InvalidHistory("conformance state is not initialized".to_owned())
            })?;
        if state.report.record_count != durable_count {
            return Err(ConformanceError::InvalidHistory(
                "durable ledger and cached conformance state diverged".to_owned(),
            ));
        }
        state.require_complete_terminal_pair()?;
        Ok(state.report.clone())
    }

    #[must_use]
    pub const fn inner(&self) -> &Ledger<C, F> {
        &self.ledger
    }

    fn next_record(
        &self,
        timestamp: &str,
        kind: AuditKind,
        payload: LosslessJson,
    ) -> Result<AuditRecord, ConformanceError> {
        let projection = self.ledger.projection()?;
        Ok(AuditRecord::new(
            self.ledger.next_sequence(),
            timestamp,
            projection.run_id,
            projection.run_generation,
            kind,
            payload,
            self.ledger.previous_hash(),
        )?)
    }

    fn append_terminal_pair(
        &mut self,
        evidence: AuditRecord,
        seal: AuditRecord,
    ) -> Result<ConformanceReport, ConformanceError> {
        self.ledger.flush()?;
        self.conformance = self.tail_conformance.clone();
        let mut candidate = self.conformance.clone().ok_or_else(|| {
            ConformanceError::InvalidHistory("conformance state is not initialized".to_owned())
        })?;
        // Terminal evidence has no public projection; preparing the seal here
        // keeps the cached conformance hash equal to the actual durable record.
        let evidence = self.ledger.prepare_event(evidence)?;
        let seal = self.ledger.prepare_event(seal)?;
        candidate.apply(&evidence)?;
        candidate.apply(&seal)?;
        candidate.require_complete_terminal_pair()?;
        self.ledger
            .append_conformance_record(evidence, AppendDurability::Required)?;
        self.ledger
            .append_conformance_record(seal, AppendDurability::Required)?;
        let report = candidate.report.clone();
        self.conformance = Some(candidate.clone());
        self.tail_conformance = Some(candidate);
        Ok(report)
    }
}

impl ConformanceState {
    fn from_verified(
        run_id: Uuid,
        records: &[AuditRecord],
        mut check_budget: impl FnMut() -> Result<(), ConformanceError>,
    ) -> Result<Self, ConformanceError> {
        let report = verify_ledger_records_with_check(run_id, records, &mut check_budget)?;
        let mut active_turn = None;
        let mut pending = BTreeSet::new();
        let mut runtime_intents = BTreeMap::new();
        let mut caller_native_intent_seen = false;
        for (index, record) in records.iter().enumerate() {
            caller_native_intent_seen |= caller_native_intent(record)?;
            match record.kind() {
                AuditKind::IdempotencyReserved => {
                    record_idempotency_intent(index, record, &mut runtime_intents)?;
                }
                AuditKind::TurnStarted => {
                    active_turn = Some(required_string(record.payload(), "turn_id")?);
                }
                AuditKind::TurnTerminal | AuditKind::OutcomeUnknown => active_turn = None,
                AuditKind::LifecycleTransition
                    if required_string(record.payload(), "current")? == "paused" =>
                {
                    active_turn = None;
                }
                AuditKind::InteractionOpened => {
                    pending.insert(required_string(record.payload(), "request_id")?);
                }
                AuditKind::InteractionResolved => {
                    pending.remove(&required_string(record.payload(), "request_id")?);
                }
                _ => {}
            }
            check_budget()?;
        }
        check_budget()?;
        Ok(Self {
            report,
            previous_generation: records.last().map_or(0, AuditRecord::run_generation),
            previous_kind: records.last().map(AuditRecord::kind),
            active_turn,
            pending,
            runtime_intents,
            caller_native_intent_seen,
        })
    }

    fn apply(&mut self, record: &AuditRecord) -> Result<(), ConformanceError> {
        self.caller_native_intent_seen |= caller_native_intent(record)?;
        let expected_sequence = self.report.record_count.saturating_add(1);
        if record.sequence() != expected_sequence
            || record.run_id() != self.report.run_id
            || record.previous_hash() != self.report.head_hash
        {
            return Err(ConformanceError::InvalidHistory(format!(
                "record {expected_sequence} breaks identity, sequence, or hash continuity"
            )));
        }
        if record.run_generation() < self.previous_generation {
            return Err(ConformanceError::InvalidHistory(format!(
                "record {expected_sequence} regresses run generation"
            )));
        }
        let line = record.canonical_line()?;
        let reparsed = AuditRecord::from_canonical_line(&line[..line.len() - 1])?;
        if reparsed.canonical_line()? != line {
            return Err(ConformanceError::InvalidHistory(format!(
                "record {expected_sequence} is not a canonical fixed point"
            )));
        }
        if self.report.terminal_sealed {
            return Err(ConformanceError::InvalidHistory(
                "record follows a terminal seal".to_owned(),
            ));
        }
        if matches!(
            self.previous_kind,
            Some(AuditKind::StartFailed | AuditKind::CleanupResult)
        ) && record.kind() != AuditKind::LifecycleTransition
        {
            return Err(ConformanceError::InvalidHistory(
                "terminal evidence is not immediately followed by its seal".to_owned(),
            ));
        }
        if matches!(
            record.kind(),
            AuditKind::WorkspaceInitialized
                | AuditKind::RunCreated
                | AuditKind::WriteContinuationCreated
        ) {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap kind repeats after allocation".to_owned(),
            ));
        }
        match record.kind() {
            AuditKind::IdempotencyReserved => {
                record_runtime_operation_intent(&mut self.runtime_intents, record)?;
            }
            AuditKind::TurnStarted => {
                let turn_id = required_string(record.payload(), "turn_id")?;
                if !utf8_bounded(&turn_id, 256) || self.active_turn.is_some() {
                    return Err(ConformanceError::InvalidHistory(
                        "turn_started does not name one inactive bounded turn".to_owned(),
                    ));
                }
                self.report.lifecycle =
                    checked_implicit_transition(self.report.lifecycle, RunLifecycle::Running)?;
                self.active_turn = Some(turn_id);
            }
            AuditKind::TurnTerminal => {
                let turn_id = required_string(record.payload(), "turn_id")?;
                if self.active_turn.as_deref() != Some(turn_id.as_str()) {
                    return Err(ConformanceError::InvalidHistory(
                        "turn_terminal does not match the active turn".to_owned(),
                    ));
                }
                self.active_turn = None;
                let target = if self.pending.is_empty() {
                    RunLifecycle::Idle
                } else {
                    RunLifecycle::WaitingInteraction
                };
                self.report.lifecycle = checked_implicit_transition(self.report.lifecycle, target)?;
            }
            AuditKind::InteractionOpened => {
                let request_id = required_string(record.payload(), "request_id")?;
                if !utf8_bounded(&request_id, 256) || !self.pending.insert(request_id) {
                    return Err(ConformanceError::InvalidHistory(
                        "interaction_opened is duplicate or unbounded".to_owned(),
                    ));
                }
                if self.report.lifecycle != RunLifecycle::WaitingInteraction {
                    self.report.lifecycle = checked_implicit_transition(
                        self.report.lifecycle,
                        RunLifecycle::WaitingInteraction,
                    )?;
                }
            }
            AuditKind::InteractionResolved => {
                let request_id = required_string(record.payload(), "request_id")?;
                if !self.pending.remove(&request_id) {
                    return Err(ConformanceError::InvalidHistory(
                        "interaction_resolved does not match an open interaction".to_owned(),
                    ));
                }
                let target = if self.pending.is_empty() {
                    if self.active_turn.is_some() {
                        RunLifecycle::Running
                    } else {
                        RunLifecycle::Idle
                    }
                } else {
                    RunLifecycle::WaitingInteraction
                };
                if self.report.lifecycle != target {
                    self.report.lifecycle =
                        checked_implicit_transition(self.report.lifecycle, target)?;
                }
            }
            AuditKind::Reconciliation => {
                self.report.lifecycle = checked_implicit_transition(
                    self.report.lifecycle,
                    RunLifecycle::ReconciliationRequired,
                )?;
            }
            AuditKind::OutcomeUnknown => {
                self.report.lifecycle =
                    checked_outcome_unknown(self.report.lifecycle, self.caller_native_intent_seen)?;
                self.active_turn = None;
            }
            AuditKind::StartFailed => {
                if self.report.lifecycle != RunLifecycle::Starting {
                    return Err(ConformanceError::InvalidHistory(
                        "start_failed evidence is outside starting".to_owned(),
                    ));
                }
                ensure_object_keys(record.payload(), &["reason"])?;
                if !utf8_bounded(&required_string(record.payload(), "reason")?, 1024) {
                    return Err(ConformanceError::InvalidHistory(
                        "start_failed reason is outside its bound".to_owned(),
                    ));
                }
            }
            AuditKind::CleanupResult => {
                ensure_object_keys(record.payload(), &["outcome"])?;
                if !utf8_bounded(&required_string(record.payload(), "outcome")?, 1024) {
                    return Err(ConformanceError::InvalidHistory(
                        "closed seal cleanup outcome is outside its bound".to_owned(),
                    ));
                }
            }
            AuditKind::LifecycleTransition => self.apply_transition(record)?,
            _ => {}
        }
        self.report.record_count = expected_sequence;
        self.report.head_hash = record.hash().to_owned();
        self.previous_generation = record.run_generation();
        self.previous_kind = Some(record.kind());
        Ok(())
    }

    fn apply_transition(&mut self, record: &AuditRecord) -> Result<(), ConformanceError> {
        let has_interrupt_confirmation =
            object_has_key(record.payload(), "interrupt_terminal_confirmed")?;
        if has_interrupt_confirmation {
            ensure_object_keys(
                record.payload(),
                &[
                    "previous",
                    "current",
                    "terminal_seal",
                    "interrupt_terminal_confirmed",
                ],
            )?;
        } else {
            ensure_object_keys(record.payload(), &["previous", "current", "terminal_seal"])?;
        }
        let previous = parse_lifecycle(&required_string(record.payload(), "previous")?)?;
        let current = parse_lifecycle(&required_string(record.payload(), "current")?)?;
        let terminal_seal = required_bool(record.payload(), "terminal_seal")?;
        let interrupt_confirmed = optional_bool(record.payload(), "interrupt_terminal_confirmed")?;
        if previous != self.report.lifecycle || !lifecycle_transition_allowed(previous, current) {
            return Err(ConformanceError::InvalidHistory(
                "lifecycle transition is not allowed from reconstructed state".to_owned(),
            ));
        }
        let needs_confirmation =
            matches!(
                previous,
                RunLifecycle::Running | RunLifecycle::WaitingInteraction
            ) && matches!(current, RunLifecycle::Paused | RunLifecycle::Closed);
        if interrupt_confirmed != needs_confirmation.then_some(true) {
            return Err(ConformanceError::InvalidHistory(
                "interrupt terminal confirmation does not match the direct edge".to_owned(),
            ));
        }
        let terminal = matches!(current, RunLifecycle::Closed | RunLifecycle::StartFailed);
        if terminal != terminal_seal {
            return Err(ConformanceError::InvalidHistory(
                "terminal_seal does not match transition finality".to_owned(),
            ));
        }
        if current == RunLifecycle::StartFailed
            && self.previous_kind != Some(AuditKind::StartFailed)
        {
            return Err(ConformanceError::InvalidHistory(
                "start_failed seal lacks adjacent failure evidence".to_owned(),
            ));
        }
        if current == RunLifecycle::Closed && self.previous_kind != Some(AuditKind::CleanupResult) {
            return Err(ConformanceError::InvalidHistory(
                "closed seal lacks adjacent cleanup evidence".to_owned(),
            ));
        }
        self.report.lifecycle = current;
        if current == RunLifecycle::Paused {
            self.active_turn = None;
        }
        self.report.terminal_sealed = terminal;
        Ok(())
    }

    fn require_complete_terminal_pair(&self) -> Result<(), ConformanceError> {
        if matches!(
            self.previous_kind,
            Some(AuditKind::StartFailed | AuditKind::CleanupResult)
        ) {
            return Err(ConformanceError::InvalidHistory(
                "terminal evidence is missing its terminal seal".to_owned(),
            ));
        }
        Ok(())
    }
}

pub fn verify_ledger_records(
    run_id: Uuid,
    records: &[AuditRecord],
) -> Result<ConformanceReport, ConformanceError> {
    verify_ledger_records_with_check(run_id, records, &mut || Ok(()))
}

fn verify_ledger_records_with_check(
    run_id: Uuid,
    records: &[AuditRecord],
    check_budget: &mut impl FnMut() -> Result<(), ConformanceError>,
) -> Result<ConformanceReport, ConformanceError> {
    check_budget()?;
    if run_id.get_version_num() != 7 {
        return Err(ConformanceError::InvalidHistory(
            "run identity must be UUIDv7".to_owned(),
        ));
    }
    if records.is_empty() {
        return Ok(ConformanceReport {
            run_id,
            record_count: 0,
            lifecycle: RunLifecycle::Starting,
            terminal_sealed: false,
            bootstrap_kind: None,
            idempotency_intent: None,
            head_hash: GENESIS_PREVIOUS_HASH.to_owned(),
        });
    }
    if records.len() < 3 {
        return Err(ConformanceError::InvalidHistory(
            "ledger contains an incomplete bootstrap prefix".to_owned(),
        ));
    }
    let expected_prefix = [
        AuditKind::WorkspaceInitialized,
        AuditKind::IdempotencyReserved,
    ];
    for (index, kind) in expected_prefix.into_iter().enumerate() {
        if records[index].kind() != kind {
            return Err(ConformanceError::InvalidHistory(format!(
                "bootstrap record {} has the wrong kind",
                index + 1
            )));
        }
    }
    let bootstrap_kind = match records[2].kind() {
        AuditKind::RunCreated => BootstrapRecordKind::RunCreated,
        AuditKind::WriteContinuationCreated => BootstrapRecordKind::WriteContinuationCreated,
        _ => {
            return Err(ConformanceError::InvalidHistory(
                "third bootstrap record must allocate the run".to_owned(),
            ));
        }
    };
    let workspace_id = required_string(records[0].payload(), "workspace_id")?;
    ensure_object_keys(records[0].payload(), &["workspace_id"])?;
    if !is_sha256(&workspace_id) {
        return Err(ConformanceError::InvalidHistory(
            "workspace bootstrap identity is invalid".to_owned(),
        ));
    }
    let intent = intent_from_payload(records[1].payload())?;
    if intent.run_id != run_id {
        return Err(ConformanceError::InvalidHistory(
            "idempotency intent targets another run".to_owned(),
        ));
    }
    if records[..3]
        .iter()
        .any(|record| record.run_generation() != 0)
    {
        return Err(ConformanceError::InvalidHistory(
            "bootstrap records must use run generation zero".to_owned(),
        ));
    }
    let valid_bootstrap_operation = match bootstrap_kind {
        BootstrapRecordKind::RunCreated => matches!(
            intent.operation,
            IdempotencyOperation::StartRun | IdempotencyOperation::ForkRun
        ),
        BootstrapRecordKind::WriteContinuationCreated => {
            intent.operation == IdempotencyOperation::CreateWriteContinuation
        }
    };
    if !valid_bootstrap_operation {
        return Err(ConformanceError::InvalidHistory(
            "allocation kind does not match the idempotency operation".to_owned(),
        ));
    }
    let mut lifecycle = RunLifecycle::Starting;
    let mut sealed = false;
    let mut previous_hash = GENESIS_PREVIOUS_HASH;
    let mut previous_generation = 0;
    let mut active_turn: Option<String> = None;
    let mut pending = BTreeSet::new();
    let mut runtime_intents = BTreeMap::new();
    let mut caller_native_intent_seen = false;
    for (index, record) in records.iter().enumerate() {
        caller_native_intent_seen |= caller_native_intent(record)?;
        let expected_sequence = u64::try_from(index).unwrap_or(u64::MAX) + 1;
        if record.sequence() != expected_sequence
            || record.run_id() != run_id
            || record.previous_hash() != previous_hash
        {
            return Err(ConformanceError::InvalidHistory(format!(
                "record {} breaks identity, sequence, or hash continuity",
                index + 1
            )));
        }
        if index > 0 && record.run_generation() < previous_generation {
            return Err(ConformanceError::InvalidHistory(format!(
                "record {} regresses run generation",
                index + 1
            )));
        }
        let line = record.canonical_line()?;
        let reparsed = AuditRecord::from_canonical_line(&line[..line.len() - 1])?;
        if reparsed.canonical_line()? != line {
            return Err(ConformanceError::InvalidHistory(format!(
                "record {} is not a canonical fixed point",
                index + 1
            )));
        }
        if sealed {
            return Err(ConformanceError::InvalidHistory(
                "record follows a terminal seal".to_owned(),
            ));
        }
        if index > 0
            && matches!(
                records[index - 1].kind(),
                AuditKind::StartFailed | AuditKind::CleanupResult
            )
            && record.kind() != AuditKind::LifecycleTransition
        {
            return Err(ConformanceError::InvalidHistory(
                "terminal evidence is not immediately followed by its seal".to_owned(),
            ));
        }
        if index >= 3
            && matches!(
                record.kind(),
                AuditKind::WorkspaceInitialized
                    | AuditKind::RunCreated
                    | AuditKind::WriteContinuationCreated
            )
        {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap kind repeats after allocation".to_owned(),
            ));
        }
        match record.kind() {
            AuditKind::IdempotencyReserved => {
                record_idempotency_intent(index, record, &mut runtime_intents)?;
            }
            AuditKind::TurnStarted => {
                let turn_id = required_string(record.payload(), "turn_id")?;
                if !utf8_bounded(&turn_id, 256) || active_turn.is_some() {
                    return Err(ConformanceError::InvalidHistory(
                        "turn_started does not name one inactive bounded turn".to_owned(),
                    ));
                }
                lifecycle = checked_implicit_transition(lifecycle, RunLifecycle::Running)?;
                active_turn = Some(turn_id);
            }
            AuditKind::TurnTerminal => {
                let turn_id = required_string(record.payload(), "turn_id")?;
                if active_turn.as_deref() != Some(turn_id.as_str()) {
                    return Err(ConformanceError::InvalidHistory(
                        "turn_terminal does not match the active turn".to_owned(),
                    ));
                }
                active_turn = None;
                let target = if pending.is_empty() {
                    RunLifecycle::Idle
                } else {
                    RunLifecycle::WaitingInteraction
                };
                lifecycle = checked_implicit_transition(lifecycle, target)?;
            }
            AuditKind::InteractionOpened => {
                let request_id = required_string(record.payload(), "request_id")?;
                if !utf8_bounded(&request_id, 256) || !pending.insert(request_id) {
                    return Err(ConformanceError::InvalidHistory(
                        "interaction_opened is duplicate or unbounded".to_owned(),
                    ));
                }
                if lifecycle != RunLifecycle::WaitingInteraction {
                    lifecycle =
                        checked_implicit_transition(lifecycle, RunLifecycle::WaitingInteraction)?;
                }
            }
            AuditKind::InteractionResolved => {
                let request_id = required_string(record.payload(), "request_id")?;
                if !pending.remove(&request_id) {
                    return Err(ConformanceError::InvalidHistory(
                        "interaction_resolved does not match an open interaction".to_owned(),
                    ));
                }
                let target = if pending.is_empty() {
                    if active_turn.is_some() {
                        RunLifecycle::Running
                    } else {
                        RunLifecycle::Idle
                    }
                } else {
                    RunLifecycle::WaitingInteraction
                };
                if lifecycle != target {
                    lifecycle = checked_implicit_transition(lifecycle, target)?;
                }
            }
            AuditKind::Reconciliation => {
                lifecycle =
                    checked_implicit_transition(lifecycle, RunLifecycle::ReconciliationRequired)?;
            }
            AuditKind::OutcomeUnknown => {
                lifecycle = checked_outcome_unknown(lifecycle, caller_native_intent_seen)?;
                active_turn = None;
            }
            AuditKind::StartFailed => {
                if lifecycle != RunLifecycle::Starting {
                    return Err(ConformanceError::InvalidHistory(
                        "start_failed evidence is outside starting".to_owned(),
                    ));
                }
                ensure_object_keys(record.payload(), &["reason"])?;
                let reason = required_string(record.payload(), "reason")?;
                if !utf8_bounded(&reason, 1024) {
                    return Err(ConformanceError::InvalidHistory(
                        "start_failed reason is outside its bound".to_owned(),
                    ));
                }
            }
            AuditKind::LifecycleTransition => {
                let has_interrupt_confirmation =
                    object_has_key(record.payload(), "interrupt_terminal_confirmed")?;
                if has_interrupt_confirmation {
                    ensure_object_keys(
                        record.payload(),
                        &[
                            "previous",
                            "current",
                            "terminal_seal",
                            "interrupt_terminal_confirmed",
                        ],
                    )?;
                } else {
                    ensure_object_keys(
                        record.payload(),
                        &["previous", "current", "terminal_seal"],
                    )?;
                }
                let previous = parse_lifecycle(&required_string(record.payload(), "previous")?)?;
                let current = parse_lifecycle(&required_string(record.payload(), "current")?)?;
                let terminal_seal = required_bool(record.payload(), "terminal_seal")?;
                let interrupt_confirmed =
                    optional_bool(record.payload(), "interrupt_terminal_confirmed")?;
                if previous != lifecycle || !lifecycle_transition_allowed(previous, current) {
                    return Err(ConformanceError::InvalidHistory(
                        "lifecycle transition is not allowed from reconstructed state".to_owned(),
                    ));
                }
                let requires_interrupt_confirmation =
                    matches!(
                        previous,
                        RunLifecycle::Running | RunLifecycle::WaitingInteraction
                    ) && matches!(current, RunLifecycle::Paused | RunLifecycle::Closed);
                if interrupt_confirmed != requires_interrupt_confirmation.then_some(true) {
                    return Err(ConformanceError::InvalidHistory(
                        "interrupt terminal confirmation does not match the direct edge".to_owned(),
                    ));
                }
                let terminal = matches!(current, RunLifecycle::Closed | RunLifecycle::StartFailed);
                if terminal != terminal_seal {
                    return Err(ConformanceError::InvalidHistory(
                        "terminal_seal does not match transition finality".to_owned(),
                    ));
                }
                if current == RunLifecycle::StartFailed
                    && index
                        .checked_sub(1)
                        .and_then(|prior| records.get(prior))
                        .map(AuditRecord::kind)
                        != Some(AuditKind::StartFailed)
                {
                    return Err(ConformanceError::InvalidHistory(
                        "start_failed seal lacks adjacent failure evidence".to_owned(),
                    ));
                }
                if current == RunLifecycle::Closed
                    && index
                        .checked_sub(1)
                        .and_then(|prior| records.get(prior))
                        .map(AuditRecord::kind)
                        != Some(AuditKind::CleanupResult)
                {
                    return Err(ConformanceError::InvalidHistory(
                        "closed seal lacks adjacent cleanup evidence".to_owned(),
                    ));
                }
                if current == RunLifecycle::Closed {
                    ensure_object_keys(records[index - 1].payload(), &["outcome"])?;
                    let outcome = required_string(records[index - 1].payload(), "outcome")?;
                    if !utf8_bounded(&outcome, 1024) {
                        return Err(ConformanceError::InvalidHistory(
                            "closed seal cleanup outcome is outside its bound".to_owned(),
                        ));
                    }
                }
                lifecycle = current;
                if current == RunLifecycle::Paused {
                    active_turn = None;
                }
                sealed = terminal;
            }
            _ => {}
        }
        previous_hash = record.hash();
        previous_generation = record.run_generation();
        check_budget()?;
    }
    check_budget()?;
    if records.last().is_some_and(|record| {
        matches!(
            record.kind(),
            AuditKind::StartFailed | AuditKind::CleanupResult
        )
    }) {
        return Err(ConformanceError::InvalidHistory(
            "terminal evidence is missing its terminal seal".to_owned(),
        ));
    }
    Ok(ConformanceReport {
        run_id,
        record_count: u64::try_from(records.len()).unwrap_or(u64::MAX),
        lifecycle,
        terminal_sealed: sealed,
        bootstrap_kind: Some(bootstrap_kind),
        idempotency_intent: Some(intent),
        head_hash: previous_hash.to_owned(),
    })
}

pub fn delete_run_confirmed(
    state_root: &Path,
    run_id: Uuid,
    confirmed: bool,
) -> Result<(), ConformanceError> {
    delete_run_confirmed_inner(state_root, run_id, confirmed, || {})
}

fn delete_run_confirmed_inner(
    state_root: &Path,
    run_id: Uuid,
    confirmed: bool,
    before_remove: impl FnOnce(),
) -> Result<(), ConformanceError> {
    if !confirmed {
        return Err(ConformanceError::ConfirmationRequired);
    }
    let runs_root = state_root.join("runs");
    let run_root = runs_root.join(run_id.to_string());
    let uid = SystemWorkspacePlatform.current_uid();
    let state_identity = secure_directory_identity(state_root, uid)?;
    let runs_identity = secure_directory_identity(&runs_root, uid)?;
    let run_identity = secure_directory_identity(&run_root, uid)?;
    let _workspace_lease = crate::ledger_generation::WorkspaceLease::for_run(&run_root)?;
    verify_delete_terminal(&run_root, run_id)?;
    if secure_directory_identity(state_root, uid)? != state_identity
        || secure_directory_identity(&runs_root, uid)? != runs_identity
        || secure_directory_identity(&run_root, uid)? != run_identity
    {
        return Err(ConformanceError::Security(
            "state, runs, or run directory identity changed before deletion".to_owned(),
        ));
    }
    before_remove();
    fs::remove_dir_all(&run_root)?;
    sync_directory(&runs_root)?;
    Ok(())
}

pub(crate) fn verify_delete_terminal(
    run_root: &Path,
    run_id: Uuid,
) -> Result<RunLifecycle, ConformanceError> {
    let snapshot = capture_terminal_escape(run_root, run_id)?;
    let lifecycle = snapshot.state.lifecycle;
    let verified = (|| {
        let ledger = crate::ledger::ObservedLedger::open_complete(
            run_root,
            run_id,
            snapshot.state.ledger_head.sequence,
        )?;
        let state = ConformanceState::from_verified(run_id, ledger.records(), || Ok(()))?;
        state.require_complete_terminal_pair()?;
        let report = state.report;
        Ok(report.terminal_sealed
            && report.lifecycle == snapshot.state.lifecycle
            && report.head_hash == snapshot.state.ledger_head.hash)
    })();
    let terminal = match verified {
        Ok(terminal) => terminal,
        Err(ConformanceError::Ledger(LedgerError::Integrity(_)))
        | Err(ConformanceError::InvalidHistory(_)) => {
            terminal_state_escape(run_root, run_id, snapshot)?
        }
        Err(error) => return Err(error),
    };
    if !terminal {
        return Err(ConformanceError::DeleteNotTerminal);
    }
    Ok(lifecycle)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

struct TerminalEscapeSnapshot {
    state: RunStateProjection,
    final_record: AuditRecord,
    state_file: DirectoryIdentity,
    audit_file: DirectoryIdentity,
    selected_append: (PathBuf, Option<String>),
}

fn secure_directory_identity(path: &Path, uid: u32) -> Result<DirectoryIdentity, ConformanceError> {
    verify_secure_directory(path, uid).map_err(security_error)?;
    file_identity(path)
}

fn file_identity(path: &Path) -> Result<DirectoryIdentity, ConformanceError> {
    let metadata = fs::symlink_metadata(path)?;
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn capture_terminal_escape(
    run_root: &Path,
    run_id: Uuid,
) -> Result<TerminalEscapeSnapshot, ConformanceError> {
    let state_path = run_root.join("state.json");
    verify_secure_file(&state_path, SystemWorkspacePlatform.current_uid())
        .map_err(security_error)?;
    let metadata = fs::metadata(&state_path)?;
    if metadata.len() > MAX_STATE_BYTES {
        return Err(ConformanceError::Security(
            "state projection exceeds its bound".to_owned(),
        ));
    }
    let bytes = fs::read(&state_path)?;
    let body = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let text = std::str::from_utf8(body)
        .map_err(|_| ConformanceError::Security("state projection is not UTF-8".to_owned()))?;
    let parsed = parse(text).map_err(|error| ConformanceError::Security(error.to_string()))?;
    if canonicalize(&parsed).map_err(|error| ConformanceError::Security(error.to_string()))? != body
    {
        return Err(ConformanceError::Security(
            "state projection is not canonical".to_owned(),
        ));
    }
    let state: RunStateProjection = serde_json::from_slice(body)
        .map_err(|error| ConformanceError::Security(error.to_string()))?;
    if state.run_id != run_id
        || !matches!(
            state.lifecycle,
            RunLifecycle::Closed | RunLifecycle::StartFailed
        )
    {
        return Err(ConformanceError::DeleteNotTerminal);
    }
    let selected_append =
        crate::ledger_generation::selected_terminal_record_path(run_root, run_id)?;
    let final_record = read_final_record(&selected_append.0)?;
    Ok(TerminalEscapeSnapshot {
        state,
        final_record,
        state_file: file_identity(&state_path)?,
        audit_file: file_identity(&selected_append.0)?,
        selected_append,
    })
}

fn terminal_state_escape(
    run_root: &Path,
    run_id: Uuid,
    snapshot: TerminalEscapeSnapshot,
) -> Result<bool, ConformanceError> {
    if file_identity(&run_root.join("state.json"))? != snapshot.state_file
        || crate::ledger_generation::selected_terminal_record_path(run_root, run_id)?
            != snapshot.selected_append
        || file_identity(&snapshot.selected_append.0)? != snapshot.audit_file
    {
        return Err(ConformanceError::Security(
            "terminal escape evidence changed during verification".to_owned(),
        ));
    }
    let state = snapshot.state;
    let final_record = snapshot.final_record;
    if final_record.run_id() != run_id
        || final_record.sequence() != state.ledger_head.sequence
        || final_record.hash() != state.ledger_head.hash
        || final_record.kind() != AuditKind::LifecycleTransition
        || !required_bool(final_record.payload(), "terminal_seal")?
    {
        return Ok(false);
    }
    let previous = parse_lifecycle(&required_string(final_record.payload(), "previous")?)?;
    let sealed = parse_lifecycle(&required_string(final_record.payload(), "current")?)?;
    let interrupt_confirmed =
        optional_bool(final_record.payload(), "interrupt_terminal_confirmed")?;
    let requires_interrupt_confirmation = matches!(
        previous,
        RunLifecycle::Running | RunLifecycle::WaitingInteraction
    ) && sealed == RunLifecycle::Closed;
    if interrupt_confirmed != requires_interrupt_confirmation.then_some(true)
        || !lifecycle_transition_allowed(previous, sealed)
    {
        return Ok(false);
    }
    if interrupt_confirmed.is_some() {
        ensure_object_keys(
            final_record.payload(),
            &[
                "previous",
                "current",
                "terminal_seal",
                "interrupt_terminal_confirmed",
            ],
        )?;
    } else {
        ensure_object_keys(
            final_record.payload(),
            &["previous", "current", "terminal_seal"],
        )?;
    }
    Ok(sealed == state.lifecycle
        && matches!(sealed, RunLifecycle::Closed | RunLifecycle::StartFailed))
}

fn read_final_record(path: &Path) -> Result<AuditRecord, ConformanceError> {
    verify_secure_file(path, SystemWorkspacePlatform.current_uid()).map_err(security_error)?;
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length == 0 {
        return Err(ConformanceError::DeleteNotTerminal);
    }
    let window = u64::try_from(MAX_AUDIT_LINE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        .min(length);
    file.seek(SeekFrom::Start(length - window))?;
    let mut bytes = Vec::with_capacity(usize::try_from(window).unwrap_or(MAX_AUDIT_LINE_BYTES));
    file.take(window).read_to_end(&mut bytes)?;
    if bytes.last() != Some(&b'\n') {
        return Err(ConformanceError::DeleteNotTerminal);
    }
    bytes.pop();
    let start = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let line = &bytes[start..];
    if line.is_empty() || line.len() > MAX_AUDIT_LINE_BYTES {
        return Err(ConformanceError::DeleteNotTerminal);
    }
    AuditRecord::from_canonical_line(line).map_err(ConformanceError::from)
}

#[must_use]
pub fn lifecycle_transition_allowed(previous: RunLifecycle, current: RunLifecycle) -> bool {
    matches!(
        (previous, current),
        (
            RunLifecycle::Starting,
            RunLifecycle::Idle | RunLifecycle::StartFailed
        ) | (
            RunLifecycle::Idle,
            RunLifecycle::Running | RunLifecycle::Paused | RunLifecycle::Closed
        ) | (
            RunLifecycle::Running,
            RunLifecycle::Idle
                | RunLifecycle::WaitingInteraction
                | RunLifecycle::ReconciliationRequired
                | RunLifecycle::OutcomeUnknown
                | RunLifecycle::Paused
                | RunLifecycle::Closed
        ) | (
            RunLifecycle::WaitingInteraction,
            RunLifecycle::Running
                | RunLifecycle::Idle
                | RunLifecycle::ReconciliationRequired
                | RunLifecycle::OutcomeUnknown
                | RunLifecycle::Paused
                | RunLifecycle::Closed
        ) | (
            RunLifecycle::ReconciliationRequired,
            RunLifecycle::Paused | RunLifecycle::OutcomeUnknown
        ) | (
            RunLifecycle::Paused,
            RunLifecycle::Idle | RunLifecycle::Closed
        ) | (
            RunLifecycle::OutcomeUnknown,
            RunLifecycle::ReconciliationRequired | RunLifecycle::Paused | RunLifecycle::Closed
        )
    )
}

fn checked_implicit_transition(
    previous: RunLifecycle,
    current: RunLifecycle,
) -> Result<RunLifecycle, ConformanceError> {
    if lifecycle_transition_allowed(previous, current) {
        Ok(current)
    } else {
        Err(ConformanceError::InvalidHistory(format!(
            "record implies forbidden lifecycle transition {} -> {}",
            previous.as_str(),
            current.as_str()
        )))
    }
}

fn utf8_bounded(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum
}

fn intent_from_payload(payload: &LosslessJson) -> Result<IdempotencyIntent, ConformanceError> {
    let bytes = canonicalize(payload)
        .map_err(|error| ConformanceError::InvalidIntent(error.to_string()))?;
    let intent: IdempotencyIntent = serde_json::from_slice(&bytes)
        .map_err(|error| ConformanceError::InvalidIntent(error.to_string()))?;
    intent.validate()?;
    Ok(intent)
}

fn runtime_operation_intent(
    record: &AuditRecord,
) -> Result<RuntimeOperationIntent, ConformanceError> {
    let bytes = canonicalize(record.payload())
        .map_err(|error| ConformanceError::InvalidHistory(error.to_string()))?;
    let intent: RuntimeOperationIntent = serde_json::from_slice(&bytes).map_err(|_| {
        ConformanceError::InvalidHistory(
            "runtime idempotency intent violates its closed shape".to_owned(),
        )
    })?;
    if intent.operation_id.get_version_num() != 7
        || !utf8_bounded(&intent.idempotency_key, 256)
        || !is_sha256(&intent.request_sha256)
        || intent.run_generation == 0
        || intent.run_generation != record.run_generation()
        || !is_sha256(&intent.server_key)
        || intent.server_epoch == 0
        || intent
            .accepted_input
            .as_ref()
            .is_some_and(|input| !runtime_accepted_input_valid(input))
    {
        return Err(ConformanceError::InvalidHistory(
            "runtime idempotency intent violates the checked shape".to_owned(),
        ));
    }
    Ok(intent)
}

fn runtime_accepted_input_valid(input: &RuntimeAcceptedUserInput) -> bool {
    let storage = input.content.storage();
    let content_length_valid = match &input.content {
        RuntimeAcceptedInputContent::Inline { .. } => storage.byte_length <= 1024 * 1024,
        RuntimeAcceptedInputContent::Artifact { .. } => {
            storage.byte_length > 1024 * 1024 && storage.byte_length <= 8 * 1024 * 1024
        }
    };
    content_length_valid
        && storage.artifact_id.get_version_num() == 7
        && crate::audit::is_microsecond_utc_timestamp(&storage.created_at)
        && is_sha256(&storage.sha256)
        && input.images.len() <= 64
        && input.images.iter().enumerate().all(|(ordinal, image)| {
            image.ordinal == u32::try_from(ordinal).unwrap_or(u32::MAX)
                && matches!(
                    image.detail,
                    RuntimeAcceptedImageDetail::Auto
                        | RuntimeAcceptedImageDetail::Low
                        | RuntimeAcceptedImageDetail::High
                )
                && matches!(
                    image.media_type.as_str(),
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                )
                && image.byte_length > 0
                && is_sha256(&image.sha256)
        })
}

fn record_runtime_operation_intent(
    intents: &mut BTreeMap<String, String>,
    record: &AuditRecord,
) -> Result<(), ConformanceError> {
    let intent = runtime_operation_intent(record)?;
    if intents
        .insert(intent.idempotency_key, intent.request_sha256.clone())
        .is_some_and(|existing| existing != intent.request_sha256)
    {
        return Err(ConformanceError::InvalidHistory(
            "runtime idempotency key conflicts with durable request identity".to_owned(),
        ));
    }
    Ok(())
}

fn record_idempotency_intent(
    index: usize,
    record: &AuditRecord,
    runtime_intents: &mut BTreeMap<String, String>,
) -> Result<(), ConformanceError> {
    if index == BOOTSTRAP_IDEMPOTENCY_RECORD_INDEX {
        intent_from_payload(record.payload())?;
        Ok(())
    } else {
        record_runtime_operation_intent(runtime_intents, record)
    }
}

fn parse_lifecycle(value: &str) -> Result<RunLifecycle, ConformanceError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| ConformanceError::InvalidHistory("unknown lifecycle value".to_owned()))
}

fn transition_payload(
    previous: RunLifecycle,
    current: RunLifecycle,
    terminal_seal: bool,
) -> LosslessJson {
    object(&[
        ("previous", string(previous.as_str())),
        ("current", string(current.as_str())),
        ("terminal_seal", LosslessJson::Bool(terminal_seal)),
    ])
}

fn interrupt_transition_payload(
    previous: RunLifecycle,
    current: RunLifecycle,
    terminal_seal: bool,
) -> LosslessJson {
    object(&[
        ("previous", string(previous.as_str())),
        ("current", string(current.as_str())),
        ("terminal_seal", LosslessJson::Bool(terminal_seal)),
        ("interrupt_terminal_confirmed", LosslessJson::Bool(true)),
    ])
}

fn required_string(payload: &LosslessJson, name: &str) -> Result<String, ConformanceError> {
    let LosslessJson::Object(entries) = payload else {
        return Err(ConformanceError::InvalidHistory(
            "record payload must be an object".to_owned(),
        ));
    };
    entries
        .iter()
        .find_map(|(key, value)| {
            (key == name)
                .then(|| match value {
                    LosslessJson::String(value) => Some(value.clone()),
                    _ => None,
                })
                .flatten()
        })
        .ok_or_else(|| {
            ConformanceError::InvalidHistory(format!("record payload lacks string {name}"))
        })
}

fn required_bool(payload: &LosslessJson, name: &str) -> Result<bool, ConformanceError> {
    let LosslessJson::Object(entries) = payload else {
        return Err(ConformanceError::InvalidHistory(
            "record payload must be an object".to_owned(),
        ));
    };
    entries
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value))
        .and_then(|value| match value {
            LosslessJson::Bool(value) => Some(*value),
            _ => None,
        })
        .ok_or_else(|| {
            ConformanceError::InvalidHistory(format!("record payload lacks boolean {name}"))
        })
}

fn optional_bool(payload: &LosslessJson, name: &str) -> Result<Option<bool>, ConformanceError> {
    let LosslessJson::Object(entries) = payload else {
        return Err(ConformanceError::InvalidHistory(
            "record payload must be an object".to_owned(),
        ));
    };
    match entries.iter().find(|(key, _)| key == name) {
        None => Ok(None),
        Some((_, LosslessJson::Bool(value))) => Ok(Some(*value)),
        Some(_) => Err(ConformanceError::InvalidHistory(format!(
            "record payload has non-boolean {name}"
        ))),
    }
}

fn object_has_key(payload: &LosslessJson, name: &str) -> Result<bool, ConformanceError> {
    let LosslessJson::Object(entries) = payload else {
        return Err(ConformanceError::InvalidHistory(
            "record payload must be an object".to_owned(),
        ));
    };
    Ok(entries.iter().any(|(key, _)| key == name))
}

fn ensure_object_keys(payload: &LosslessJson, expected: &[&str]) -> Result<(), ConformanceError> {
    let LosslessJson::Object(entries) = payload else {
        return Err(ConformanceError::InvalidHistory(
            "record payload must be an object".to_owned(),
        ));
    };
    if entries.len() != expected.len()
        || expected
            .iter()
            .any(|name| !entries.iter().any(|(key, _)| key == name))
    {
        return Err(ConformanceError::InvalidHistory(
            "record payload does not match its closed shape".to_owned(),
        ));
    }
    Ok(())
}

fn object(entries: &[(impl AsRef<str>, LosslessJson)]) -> LosslessJson {
    LosslessJson::Object(
        entries
            .iter()
            .map(|(key, value)| (key.as_ref().to_owned(), value.clone()))
            .collect(),
    )
}

fn string(value: &str) -> LosslessJson {
    LosslessJson::String(value.to_owned())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn security_error(error: crate::machine::MachineError) -> ConformanceError {
    ConformanceError::Security(format!("{}: {}", error.code, error.message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    const TIMESTAMP: &str = "2026-08-21T12:34:56.123456Z";

    #[test]
    fn startup_abort_history_rejects_durable_thread_dispatch_intent() {
        let run_id = Uuid::now_v7();
        let root = std::env::temp_dir().join(format!("dolgorae-abort-history-{run_id}"));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("recovery"))
            .unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        let mut ledger = ConformantLedger::open(&root, run_id).unwrap();
        ledger
            .bootstrap(&BootstrapRequest {
                timestamp: TIMESTAMP.into(),
                workspace_id: "a".repeat(64),
                intent: IdempotencyIntent {
                    schema_version: 1,
                    operation: IdempotencyOperation::StartRun,
                    idempotency_key: "caller".into(),
                    normalized_identity_sha256: "b".repeat(64),
                    run_id,
                },
                record_kind: BootstrapRecordKind::RunCreated,
                initial_access: "unknown".into(),
                default_effort: Some("low".into()),
            })
            .unwrap();
        assert_eq!(
            ledger.verify_startup_abort_history().unwrap(),
            RunLifecycle::Starting
        );
        let intent = ledger
            .next_record(
                TIMESTAMP,
                AuditKind::TurnIntent,
                crate::jcs::parse(
                    &serde_json::to_string(&CallerNativeIntent {
                        schema: "dolgorae.caller-native-intent/v1".into(),
                        operation_id: run_id,
                        method: "thread/start".into(),
                    })
                    .unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        ledger.append(intent, AppendDurability::Required).unwrap();
        assert!(matches!(
            ledger.verify_startup_abort_history(),
            Err(ConformanceError::UnauthorizedStartFailure)
        ));
        drop(ledger);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_delete_excludes_maintenance_through_removal_for_valid_and_corrupt_history() {
        use crate::ledger_generation::WorkspaceLease;
        for corrupt in [false, true] {
            let state_root =
                std::env::temp_dir().join(format!("dolgorae-delete-lease-{}", Uuid::now_v7()));
            let run_id = Uuid::now_v7();
            let run_root = state_root.join("runs").join(run_id.to_string());
            for path in [
                &state_root,
                &state_root.join("runs"),
                &run_root,
                &run_root.join("recovery"),
                &state_root.join("runtime"),
                &state_root.join("runtime/locks"),
            ] {
                fs::DirBuilder::new().mode(0o700).create(path).unwrap();
            }
            for path in [
                state_root.join("workspace.json"),
                run_root.join("audit.jsonl"),
            ] {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
                    .unwrap();
            }
            let mut ledger = ConformantLedger::open(&run_root, run_id).unwrap();
            ledger
                .bootstrap(&BootstrapRequest {
                    timestamp: TIMESTAMP.to_owned(),
                    workspace_id: "a".repeat(64),
                    intent: IdempotencyIntent {
                        schema_version: 1,
                        operation: IdempotencyOperation::StartRun,
                        idempotency_key: "delete".to_owned(),
                        normalized_identity_sha256: "b".repeat(64),
                        run_id,
                    },
                    record_kind: BootstrapRecordKind::RunCreated,
                    initial_access: "read".to_owned(),
                    default_effort: Some("medium".to_owned()),
                })
                .unwrap();
            ledger.mark_threadless_ready(TIMESTAMP).unwrap();
            ledger.seal_closed(TIMESTAMP, "worker absent").unwrap();
            drop(ledger);
            if corrupt {
                let mut bytes = fs::read(run_root.join("audit.jsonl")).unwrap();
                bytes[0] ^= 1;
                fs::write(run_root.join("audit.jsonl"), bytes).unwrap();
            }
            let mut reached_removal = false;
            delete_run_confirmed_inner(&state_root, run_id, true, || {
                reached_removal = true;
                assert!(run_root.exists());
                let root = state_root.clone();
                assert!(
                    std::thread::spawn(move || matches!(
                        WorkspaceLease::acquire(&root, true),
                        Err(LedgerError::WriterBusy(_))
                    ))
                    .join()
                    .unwrap()
                );
            })
            .unwrap();
            assert!(reached_removal);
            assert!(!run_root.exists());
            drop(WorkspaceLease::acquire(&state_root, true).unwrap());
            fs::remove_dir_all(state_root).unwrap();
        }
    }

    #[test]
    fn caller_writer_prefix_reopens_for_generation_reader() {
        use crate::turn::{DurableTurnJournal, JournalEntry, LedgerTurnJournal};
        use crate::writer::{GenerationRelease, GenerationReleasePhase as P, ReaderReservation};

        let run_id = Uuid::now_v7();
        let thread_operation = Uuid::now_v7();
        let thread_id = Uuid::now_v7().to_string();
        let state_root =
            PathBuf::from("/tmp").join(format!("dolgorae-caller-replay-{}", Uuid::now_v7()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&state_root)
            .unwrap();
        let state_root = state_root.canonicalize().unwrap();
        let root = state_root.join("runs").join(run_id.to_string());
        for path in [state_root.join("runs"), root.clone()] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("recovery"))
            .unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        let mut bootstrap = ConformantLedger::open(&root, run_id).unwrap();
        bootstrap
            .with_event_context(
                crate::event::EventAppendContext {
                    workspace_id: "a".repeat(64),
                    server_key: "c".repeat(64),
                    server_epoch: 1,
                    writer_state_revision: 0,
                },
                |ledger| {
                    ledger.bootstrap(&BootstrapRequest {
                        timestamp: TIMESTAMP.to_owned(),
                        workspace_id: "a".repeat(64),
                        intent: IdempotencyIntent {
                            schema_version: 1,
                            operation: IdempotencyOperation::StartRun,
                            idempotency_key: "caller-reader".to_owned(),
                            normalized_identity_sha256: "b".repeat(64),
                            run_id,
                        },
                        record_kind: BootstrapRecordKind::RunCreated,
                        initial_access: "unknown".to_owned(),
                        default_effort: Some("low".to_owned()),
                    })
                },
            )
            .unwrap();
        drop(bootstrap);
        // The live caller journal writes these records through Ledger, before
        // a successor Worker reopens them through ConformantLedger.
        let mut ledger = Ledger::open(&root, run_id).unwrap();
        let read_policy = serde_json::json!({
            "access":"read", "policy_epoch":1, "server_epoch":3,
            "thread_generation":1, "verification":"verified", "writer_generation":null,
        });
        let mut write_policy = read_policy.clone();
        write_policy["access"] = serde_json::json!("write");
        write_policy["policy_epoch"] = serde_json::json!(2);
        write_policy["writer_generation"] = serde_json::json!(1);
        for (kind, payload) in [
            (
                AuditKind::LifecycleTransition,
                serde_json::json!({
                    "previous":"starting", "current":"idle", "terminal_seal":false,
                }),
            ),
            (
                AuditKind::TurnIntent,
                serde_json::to_value(CallerNativeIntent {
                    schema: "dolgorae.caller-native-intent/v1".to_owned(),
                    operation_id: thread_operation,
                    method: "thread/start".to_owned(),
                })
                .unwrap(),
            ),
            (
                AuditKind::TurnIntent,
                serde_json::json!({
                    "operation_id":thread_operation, "provisional_thread_id":thread_id,
                }),
            ),
            (
                AuditKind::ThreadBound,
                serde_json::json!({
                    "operation_id":thread_operation, "thread_id":thread_id,
                    "thread_generation":1, "effective_policy":read_policy,
                }),
            ),
            (
                AuditKind::TurnIntent,
                serde_json::to_value(CallerNativeIntent {
                    schema: "dolgorae.caller-native-intent/v1".to_owned(),
                    operation_id: run_id,
                    method: "thread/settings/update".to_owned(),
                })
                .unwrap(),
            ),
            (
                AuditKind::WriterAcquired,
                serde_json::json!({
                    "transaction_id":run_id, "writer_generation":1, "effective_policy":write_policy,
                }),
            ),
        ] {
            ledger
                .with_event_context(
                    crate::event::EventAppendContext {
                        workspace_id: "a".repeat(64),
                        server_key: "c".repeat(64),
                        server_epoch: 3,
                        writer_state_revision: if kind == AuditKind::WriterAcquired {
                            2
                        } else {
                            1
                        },
                    },
                    |ledger| {
                        if kind == AuditKind::LifecycleTransition {
                            ledger.append_conformance_payload(kind, &payload, 1)
                        } else {
                            ledger.append_required_payload(kind, &payload, 1)
                        }
                    },
                )
                .unwrap();
        }
        assert_eq!(ledger.durable_records().unwrap().len(), 9);
        let records = ledger.durable_records().unwrap();
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.run_generation(), u64::from(index >= 3));
            assert_eq!(
                record.client_projection().is_some(),
                [2, 3, 8].contains(&index)
            );
        }
        let writer_event = serde_json::to_value(records[8].client_projection().unwrap()).unwrap();
        assert_eq!(writer_event["record"]["type"], "writer.state_changed");
        assert_eq!(writer_event["stamp"]["captured_head_cursor"], "9");
        assert_eq!(writer_event["stamp"]["run_state_revision"], 9);
        assert_eq!(writer_event["stamp"]["writer_state_revision"], 2);
        let operation = Uuid::now_v7();
        let controller = crate::domain::ControllerIdentity {
            controller_id: Uuid::now_v7(),
            kind: crate::domain::ControllerKind::HumanCli,
            instance_id: "caller-reader".to_owned(),
            subject_id: None,
            generation: 1,
        };
        let source = crate::writer::WriterHolder {
            run_id,
            profile: "caller-reader".to_owned(),
            controller_id: controller.controller_id,
            controller_generation: 1,
            run_generation: 1,
            worker_generation: 1,
            profile_server_key: "c".repeat(64),
            profile_server_epoch: 3,
            thread_id: Some(thread_id.clone()),
            lifecycle: RunLifecycle::Idle,
            active_turn_id: None,
            pending_interaction_count: 0,
            last_event_cursor: Some("9".to_owned()),
        };
        let release = GenerationRelease {
            purpose: crate::writer::GenerationRetirementPurpose::Release,
            transaction_id: operation,
            source,
            writer_generation: 1,
            authority_revision: 2,
            source_revision: 9,
            source_ledger_sha256: crate::jcs::sha256_hex(
                &serde_json::to_vec(&crate::projection::LedgerHead {
                    sequence: 9,
                    hash: records[8].hash().to_owned(),
                })
                .unwrap(),
            ),
            source_runtime_sha256: "1".repeat(64),
            registration_sha256: "2".repeat(64),
            scope_sha256: Some("3".repeat(64)),
            absence_sha256: Some("4".repeat(64)),
            reader_proof_sha256: None,
            retired_reader_sha256: None,
            phase: P::ReaderStarting,
            successor: Some(ReaderReservation {
                run_generation: 2,
                server_epoch: 4,
                control_socket_epoch: 3,
                attempt_started: true,
            }),
        };
        let release_evidence = serde_json::json!({
            "run_id":run_id, "thread_id":thread_id, "source_revision":9,
        });
        let release_digest =
            crate::writer::retain_release_evidence(&state_root, run_id, &release_evidence).unwrap();
        drop(ledger);
        let conformant = ConformantLedger::open(&root, run_id).unwrap_or_else(|error| {
            panic!("ConformantLedger::open at {}: {error:?}", root.display())
        });
        let checked = conformant.inner().projection().unwrap();
        assert_eq!(checked.lifecycle, RunLifecycle::Idle);
        assert_eq!(checked.thread_id.as_deref(), Some(thread_id.as_str()));
        assert_eq!(checked.run_generation, 1);
        drop(conformant);
        let plain = Ledger::open(&root, run_id)
            .unwrap_or_else(|error| panic!("Ledger::open at {}: {error:?}", root.display()));
        assert_eq!(plain.projection().unwrap(), checked);
        drop(plain);
        assert_eq!(
            crate::writer::read_release_evidence(&state_root, run_id, &release_digest).unwrap(),
            release_evidence
        );
        assert_eq!(fs::read_dir(root.join("recovery")).unwrap().count(), 0);
        let append_reader = |ledger: &mut Ledger, generation, epoch, thread_generation| {
            ledger
                .with_event_context(
                    crate::event::EventAppendContext {
                        workspace_id: "a".repeat(64),
                        server_key: "c".repeat(64),
                        server_epoch: epoch,
                        writer_state_revision: 3,
                    },
                    |ledger| {
                        let mut journal = LedgerTurnJournal::new(ledger, generation);
                        journal.append_and_sync(JournalEntry::ProvisionalThread {
                            operation_id: operation,
                            thread_id: thread_id.clone(),
                        })?;
                        journal.append_and_sync(JournalEntry::ThreadBound {
                            operation_id: operation,
                            thread_id: thread_id.clone(),
                            thread_generation,
                            effective_policy: serde_json::from_value(serde_json::json!({
                                "access":"read", "policy_epoch":thread_generation + 1,
                                "server_epoch":epoch,"thread_generation":thread_generation,
                                "verification":"verified","writer_generation":null,
                            }))
                            .unwrap(),
                        })
                    },
                )
                .unwrap();
        };
        let mut ledger = Ledger::open(&root, run_id).unwrap();
        append_reader(&mut ledger, 2, 4, 2);
        drop(ledger);
        let conformant = ConformantLedger::open(&root, run_id).unwrap();
        let records = conformant.inner().durable_records().unwrap();
        assert_eq!(records.len(), 11);
        assert_eq!(records[9].kind(), AuditKind::TurnIntent);
        assert_eq!(records[9].run_generation(), 2);
        release.verify_ledger_prefix(records).unwrap();
        let mut verified = release.clone();
        verified.phase = P::ReaderVerified;
        verified.reader_proof_sha256 = Some("5".repeat(64));
        verified.verify_ledger_prefix(records).unwrap();

        // A proven-absent Reader's records remain owned when replacement
        // reserves a later Worker generation, before that new attempt starts.
        let mut writer = crate::writer::WriterRecord::empty(&"a".repeat(64));
        let (_, generation) = writer
            .prepare_acquire(run_id, release.source.clone(), Uuid::now_v7())
            .unwrap();
        writer
            .commit_acquire(writer.transaction_id.unwrap(), generation)
            .unwrap();
        writer.prepare_release(run_id, operation).unwrap();
        writer.schema_version = 2;
        writer.generation_release = Some(verified.clone());
        writer
            .replace_absent_reader(
                &verified,
                ReaderReservation {
                    run_generation: 4,
                    server_epoch: 6,
                    control_socket_epoch: 4,
                    attempt_started: false,
                },
                "6".repeat(64),
            )
            .unwrap();
        let mut replacement = writer.generation_release.unwrap();
        replacement.verify_ledger_prefix(records).unwrap();
        let mut unretired = replacement.clone();
        unretired.retired_reader_sha256 = None;
        assert!(unretired.verify_ledger_prefix(records).is_err());
        drop(conformant);
        let mut ledger = Ledger::open(&root, run_id).unwrap();
        append_reader(&mut ledger, 4, 6, 3);
        drop(ledger);
        let conformant = ConformantLedger::open(&root, run_id).unwrap();
        let records = conformant.inner().durable_records().unwrap().to_vec();
        assert_eq!(records.len(), 13);
        assert_eq!(records[12].run_generation(), 4);
        assert!(replacement.verify_ledger_prefix(&records).is_err());
        replacement.successor.as_mut().unwrap().attempt_started = true;
        replacement.verify_ledger_prefix(&records).unwrap();
        replacement.phase = P::ReaderVerified;
        replacement.reader_proof_sha256 = Some("7".repeat(64));
        replacement.verify_ledger_prefix(&records).unwrap();
        assert_eq!(
            conformant
                .inner()
                .projection()
                .unwrap()
                .thread_id
                .as_deref(),
            Some(thread_id.as_str())
        );
        drop(conformant);

        let reject = |kind, payload: serde_json::Value, generation| {
            let mut candidate = records.clone();
            candidate.push(
                AuditRecord::new(
                    14,
                    TIMESTAMP,
                    run_id,
                    generation,
                    kind,
                    crate::jcs::parse(&serde_json::to_string(&payload).unwrap()).unwrap(),
                    records.last().unwrap().hash(),
                )
                .unwrap(),
            );
            assert!(
                replacement.verify_ledger_prefix(&candidate).is_err(),
                "{kind:?}/{generation}/{payload}"
            );
        };
        for (kind, index, thread_key) in [
            (AuditKind::TurnIntent, 11, "provisional_thread_id"),
            (AuditKind::ThreadBound, 12, "thread_id"),
        ] {
            let payload: serde_json::Value = serde_json::from_slice(
                &crate::jcs::canonicalize(records[index].payload()).unwrap(),
            )
            .unwrap();
            let mut foreign = payload.clone();
            foreign["operation_id"] = serde_json::json!(Uuid::now_v7());
            reject(kind, foreign, 4);
            let mut wrong_thread = payload.clone();
            wrong_thread[thread_key] = serde_json::json!("replacement-thread");
            reject(kind, wrong_thread, 4);
            for generation in [0, 1, 5] {
                reject(kind, payload.clone(), generation);
            }
            if kind == AuditKind::TurnIntent {
                let mut extra = payload;
                extra["input"] = serde_json::json!("must not become a model Turn");
                reject(kind, extra, 4);
            }
        }
        for phase in [P::Prepared, P::StopStarted, P::OldAbsent, P::Committed] {
            let mut wrong_phase = replacement.clone();
            wrong_phase.phase = phase;
            assert!(wrong_phase.verify_ledger_prefix(&records).is_err());
        }
        reject(
            AuditKind::TurnIntent,
            serde_json::to_value(CallerNativeIntent {
                schema: "dolgorae.caller-native-intent/v1".to_owned(),
                operation_id: operation,
                method: "thread/start".to_owned(),
            })
            .unwrap(),
            4,
        );
        let submit = crate::operation::SubmitIdentity::prepare(
            operation,
            crate::operation::Namespace::submit(&controller, &"a".repeat(64), run_id, "new-submit")
                .unwrap(),
            &crate::turn::TurnRequest {
                write: false,
                idempotency_key: "new-submit".to_owned(),
                message: "must remain refused".to_owned(),
                images: vec![],
                model: "gpt-5".to_owned(),
                effort: None,
                delivery: crate::turn::DeliveryMode::Submit,
            },
            "low",
        )
        .unwrap();
        submit.validate().unwrap();
        let mut ledger = Ledger::open(&root, run_id).unwrap();
        ledger
            .with_event_context(
                crate::event::EventAppendContext {
                    workspace_id: "a".repeat(64),
                    server_key: "c".repeat(64),
                    server_epoch: 6,
                    writer_state_revision: 3,
                },
                |ledger| {
                    LedgerTurnJournal::new(ledger, 4)
                        .append_and_sync(JournalEntry::SubmitIdentity(Box::new(submit)))
                },
            )
            .unwrap();
        assert_eq!(
            ledger.durable_records().unwrap().last().unwrap().kind(),
            AuditKind::TurnIntent
        );
        assert!(
            replacement
                .verify_ledger_prefix(ledger.durable_records().unwrap())
                .is_err()
        );
        drop(ledger);
        // Release evidence must not broaden the ledger's repair namespace.
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("recovery/foreign.json"))
            .unwrap();
        assert!(matches!(
            ConformantLedger::open(&root, run_id),
            Err(ConformanceError::Ledger(LedgerError::Integrity(reason)))
                if reason.contains("unrecognized recovery entry")
        ));
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn caller_native_intent_allows_idle_uncertainty_without_relaxing_legacy_history() {
        for caller in [false, true] {
            let run_id = Uuid::now_v7();
            let root = std::env::temp_dir()
                .join(format!("dolgorae-caller-conformance-{}", Uuid::now_v7()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join("recovery"))
                .unwrap();
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join("audit.jsonl"))
                .unwrap();
            let mut ledger = ConformantLedger::open(&root, run_id).unwrap();
            ledger
                .bootstrap(&BootstrapRequest {
                    timestamp: TIMESTAMP.to_owned(),
                    workspace_id: "a".repeat(64),
                    intent: IdempotencyIntent {
                        schema_version: 1,
                        operation: IdempotencyOperation::StartRun,
                        idempotency_key: "caller".to_owned(),
                        normalized_identity_sha256: "b".repeat(64),
                        run_id,
                    },
                    record_kind: BootstrapRecordKind::RunCreated,
                    initial_access: "unknown".to_owned(),
                    default_effort: Some("low".to_owned()),
                })
                .unwrap();
            let ready = ledger
                .next_record(
                    TIMESTAMP,
                    AuditKind::LifecycleTransition,
                    transition_payload(RunLifecycle::Starting, RunLifecycle::Idle, false),
                )
                .unwrap();
            ledger.append(ready, AppendDurability::Required).unwrap();
            if caller {
                let intent = CallerNativeIntent {
                    schema: "dolgorae.caller-native-intent/v1".to_owned(),
                    operation_id: Uuid::now_v7(),
                    method: "thread/start".to_owned(),
                };
                let record = ledger
                    .next_record(
                        TIMESTAMP,
                        AuditKind::TurnIntent,
                        parse(&serde_json::to_string(&intent).unwrap()).unwrap(),
                    )
                    .unwrap();
                ledger.append(record, AppendDurability::Required).unwrap();
            }
            let unknown = ledger
                .next_record(
                    TIMESTAMP,
                    AuditKind::OutcomeUnknown,
                    object(&[("operation_id", string(&Uuid::now_v7().to_string()))]),
                )
                .unwrap();
            let result = ledger.append(unknown, AppendDurability::Required);
            assert_eq!(result.is_ok(), caller);
            let expected = if caller {
                RunLifecycle::OutcomeUnknown
            } else {
                RunLifecycle::Idle
            };
            assert_eq!(ledger.verify().unwrap().lifecycle, expected);
            drop(ledger);
            assert_eq!(
                ConformantLedger::open(&root, run_id)
                    .unwrap()
                    .verify()
                    .unwrap()
                    .lifecycle,
                expected
            );
            fs::remove_dir_all(root).unwrap();
        }
        assert!(!lifecycle_transition_allowed(
            RunLifecycle::Idle,
            RunLifecycle::OutcomeUnknown
        ));
    }

    #[test]
    fn profile_interrupt_outcome_and_observation_are_idempotent_and_conflict_checked() {
        let run_id = Uuid::now_v7();
        let root = std::env::temp_dir().join(format!(
            "dolgorae-profile-observation-conformance-{}",
            Uuid::now_v7()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("recovery"))
            .unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        let mut ledger = ConformantLedger::open(&root, run_id).unwrap();
        ledger
            .bootstrap(&BootstrapRequest {
                timestamp: TIMESTAMP.to_owned(),
                workspace_id: "a".repeat(64),
                intent: IdempotencyIntent {
                    schema_version: 1,
                    operation: IdempotencyOperation::StartRun,
                    idempotency_key: "profile-observation".to_owned(),
                    normalized_identity_sha256: "b".repeat(64),
                    run_id,
                },
                record_kind: BootstrapRecordKind::RunCreated,
                initial_access: "read".to_owned(),
                default_effort: Some("medium".to_owned()),
            })
            .unwrap();
        for (previous, current) in [
            (RunLifecycle::Starting, RunLifecycle::Idle),
            (RunLifecycle::Idle, RunLifecycle::Running),
        ] {
            let transition = ledger
                .next_record(
                    TIMESTAMP,
                    AuditKind::LifecycleTransition,
                    transition_payload(previous, current, false),
                )
                .unwrap();
            ledger
                .append(transition, AppendDurability::Required)
                .unwrap();
        }

        let operation_id = Uuid::now_v7();
        ledger
            .mark_profile_interrupt_outcome_unknown(TIMESTAMP, operation_id)
            .unwrap();
        let after_unknown = ledger.verify().unwrap().record_count;
        ledger
            .mark_profile_interrupt_outcome_unknown(TIMESTAMP, operation_id)
            .unwrap();
        assert_eq!(ledger.verify().unwrap().record_count, after_unknown);

        let payload = serde_json::json!({
            "observation": "operator_override",
            "operation_id": operation_id,
            "profile": "default",
            "server_key": "c".repeat(64),
            "server_epoch": 1,
            "turn_id": "turn-1",
            "outcome": "outcome_unknown",
        });
        assert!(
            ledger
                .append_profile_observation(TIMESTAMP, operation_id, &payload)
                .unwrap()
        );
        assert!(
            !ledger
                .append_profile_observation(TIMESTAMP, operation_id, &payload)
                .unwrap()
        );
        let conflict = serde_json::json!({
            "observation": "operator_override",
            "operation_id": operation_id,
            "profile": "default",
            "server_key": "c".repeat(64),
            "server_epoch": 1,
            "turn_id": "turn-1",
            "outcome": "terminal_observed",
        });
        assert!(matches!(
            ledger.append_profile_observation(TIMESTAMP, operation_id, &conflict),
            Err(ConformanceError::InvalidHistory(message))
                if message == "profile observation operation ID has conflicting durable input"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_turn_intent_reopens_as_runtime_evidence() {
        let run_id = Uuid::now_v7();
        let root = std::env::temp_dir().join(format!(
            "dolgorae-runtime-intent-conformance-{}",
            Uuid::now_v7()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("recovery"))
            .unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        let mut ledger = ConformantLedger::open(&root, run_id).unwrap();
        ledger
            .bootstrap(&BootstrapRequest {
                timestamp: TIMESTAMP.to_owned(),
                workspace_id: "a".repeat(64),
                intent: IdempotencyIntent {
                    schema_version: 1,
                    operation: IdempotencyOperation::StartRun,
                    idempotency_key: "start".to_owned(),
                    normalized_identity_sha256: "b".repeat(64),
                    run_id,
                },
                record_kind: BootstrapRecordKind::RunCreated,
                initial_access: "read".to_owned(),
                default_effort: Some("medium".to_owned()),
            })
            .unwrap();
        drop(ledger);
        let mut ledger = Ledger::open(&root, run_id).unwrap();
        ledger
            .append_required_payload(
                AuditKind::IdempotencyReserved,
                &serde_json::json!({
                    "operation_id": Uuid::now_v7(),
                    "idempotency_key": "task-one",
                    "request_sha256": "c".repeat(64),
                    "run_generation": 1,
                    "server_key": "d".repeat(64),
                    "server_epoch": 1,
                    "provisional_thread": true,
                }),
                1,
            )
            .unwrap();
        drop(ledger);
        assert_eq!(
            ConformantLedger::open(&root, run_id)
                .unwrap()
                .verify()
                .unwrap()
                .record_count,
            4
        );
        let mut ledger = Ledger::open(&root, run_id).unwrap();
        ledger
            .append_required_payload(
                AuditKind::IdempotencyReserved,
                &serde_json::json!({
                    "operation_id": Uuid::now_v7(),
                    "idempotency_key": "task-one",
                    "request_sha256": "e".repeat(64),
                    "run_generation": 1,
                    "server_key": "d".repeat(64),
                    "server_epoch": 1,
                    "provisional_thread": false,
                }),
                1,
            )
            .unwrap();
        drop(ledger);
        assert!(matches!(
            ConformantLedger::open(&root, run_id),
            Err(ConformanceError::InvalidHistory(message))
                if message == "runtime idempotency key conflicts with durable request identity"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn confirmed_interrupt_capability_authors_direct_close() {
        let run_id = Uuid::now_v7();
        let root =
            std::env::temp_dir().join(format!("dolgorae-interrupt-conformance-{}", Uuid::now_v7()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("recovery"))
            .unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        let mut ledger = ConformantLedger::open(&root, run_id).unwrap();
        ledger
            .bootstrap(&BootstrapRequest {
                timestamp: TIMESTAMP.to_owned(),
                workspace_id: "a".repeat(64),
                intent: IdempotencyIntent {
                    schema_version: 1,
                    operation: IdempotencyOperation::StartRun,
                    idempotency_key: "interrupt-close".to_owned(),
                    normalized_identity_sha256: "b".repeat(64),
                    run_id,
                },
                record_kind: BootstrapRecordKind::RunCreated,
                initial_access: "read".to_owned(),
                default_effort: None,
            })
            .unwrap();
        for (previous, current) in [
            (RunLifecycle::Starting, RunLifecycle::Idle),
            (RunLifecycle::Idle, RunLifecycle::Running),
        ] {
            let transition = ledger
                .next_record(
                    TIMESTAMP,
                    AuditKind::LifecycleTransition,
                    transition_payload(previous, current, false),
                )
                .unwrap();
            ledger
                .append(transition, AppendDurability::Required)
                .unwrap();
        }
        let authority = ConfirmedInterruptAuthority::from_verified_terminal_observation(
            run_id,
            RunLifecycle::Running,
        )
        .unwrap();
        let report = ledger
            .transition_after_confirmed_interrupt(
                TIMESTAMP,
                RunLifecycle::Closed,
                Some("interrupt terminal observed"),
                authority,
            )
            .unwrap();
        assert_eq!(report.lifecycle, RunLifecycle::Closed);
        assert!(report.terminal_sealed);
        assert!(
            required_bool(
                ledger
                    .inner()
                    .durable_records()
                    .unwrap()
                    .last()
                    .unwrap()
                    .payload(),
                "interrupt_terminal_confirmed",
            )
            .unwrap()
        );
        drop(ledger);
        fs::remove_dir_all(root).unwrap();
    }
}
