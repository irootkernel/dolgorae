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
}

impl ConformantLedger<SystemLedgerClock, NoFaults> {
    pub fn open(root: impl Into<PathBuf>, run_id: Uuid) -> Result<Self, ConformanceError> {
        let ledger = Ledger::open(root, run_id)?;
        verify_ledger_records(run_id, ledger.durable_records())?;
        Ok(Self {
            ledger,
            bootstrap_recovery: false,
        })
    }

    pub fn open_for_bootstrap(
        root: impl Into<PathBuf>,
        run_id: Uuid,
    ) -> Result<Self, ConformanceError> {
        let ledger = Ledger::open(root, run_id)?;
        if ledger.durable_records().len() > 3 {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap recovery found records beyond the closed prefix".to_owned(),
            ));
        }
        Ok(Self {
            ledger,
            bootstrap_recovery: true,
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
        verify_ledger_records(run_id, ledger.durable_records())?;
        Ok(Self {
            ledger,
            bootstrap_recovery: false,
        })
    }

    pub fn open_for_bootstrap_with(
        root: impl Into<PathBuf>,
        run_id: Uuid,
        clock: C,
        faults: F,
    ) -> Result<Self, ConformanceError> {
        let ledger = Ledger::open_with(root, run_id, clock, faults)?;
        if ledger.durable_records().len() > 3 {
            return Err(ConformanceError::InvalidHistory(
                "bootstrap recovery found records beyond the closed prefix".to_owned(),
            ));
        }
        Ok(Self {
            ledger,
            bootstrap_recovery: true,
        })
    }

    pub fn bootstrap(
        &mut self,
        request: &BootstrapRequest,
    ) -> Result<ConformanceReport, ConformanceError> {
        if request.intent.run_id != self.ledger.projection().run_id
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
            previous = record.hash().to_owned();
            records.push(record);
        }
        let durable = self.ledger.durable_records();
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
        let report = verify_ledger_records(request.intent.run_id, self.ledger.durable_records())?;
        self.bootstrap_recovery = false;
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
        let mut candidate = self.ledger.durable_records().to_vec();
        candidate.push(record.clone());
        verify_ledger_records(record.run_id(), &candidate)?;
        if record.kind() == AuditKind::LifecycleTransition {
            self.ledger.append_conformance_record(record, durability)?;
        } else {
            self.ledger.append(record, durability)?;
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
        if authority.run_id != self.ledger.projection().run_id {
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
        verify_ledger_records(
            self.ledger.projection().run_id,
            self.ledger.durable_records(),
        )
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
        Ok(AuditRecord::new(
            self.ledger.next_sequence(),
            timestamp,
            self.ledger.projection().run_id,
            self.ledger.projection().run_generation,
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
        let mut candidate = self.ledger.durable_records().to_vec();
        candidate.push(evidence.clone());
        candidate.push(seal.clone());
        verify_ledger_records(evidence.run_id(), &candidate)?;
        self.ledger
            .append_conformance_record(evidence, AppendDurability::Required)?;
        self.ledger
            .append_conformance_record(seal, AppendDurability::Required)?;
        self.verify()
    }
}

pub fn verify_ledger_records(
    run_id: Uuid,
    records: &[AuditRecord],
) -> Result<ConformanceReport, ConformanceError> {
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
    let mut accepted_intents = BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
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
                let later_intent = intent_from_payload(record.payload())?;
                if later_intent.run_id != run_id {
                    return Err(ConformanceError::InvalidHistory(
                        "idempotency intent targets another run".to_owned(),
                    ));
                }
                let key = (later_intent.operation, later_intent.idempotency_key.clone());
                let identity = (
                    later_intent.normalized_identity_sha256.clone(),
                    later_intent.run_id,
                );
                if accepted_intents
                    .insert(key, identity.clone())
                    .is_some_and(|existing| existing != identity)
                {
                    return Err(ConformanceError::InvalidHistory(
                        "idempotency key conflicts with durable operation identity".to_owned(),
                    ));
                }
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
                lifecycle = checked_implicit_transition(lifecycle, RunLifecycle::OutcomeUnknown)?;
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
                sealed = terminal;
            }
            _ => {}
        }
        previous_hash = record.hash();
        previous_generation = record.run_generation();
    }
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
    if !confirmed {
        return Err(ConformanceError::ConfirmationRequired);
    }
    let runs_root = state_root.join("runs");
    let run_root = runs_root.join(run_id.to_string());
    let uid = SystemWorkspacePlatform.current_uid();
    let state_identity = secure_directory_identity(state_root, uid)?;
    let runs_identity = secure_directory_identity(&runs_root, uid)?;
    let run_identity = secure_directory_identity(&run_root, uid)?;
    let escape_snapshot = capture_terminal_escape(&run_root, run_id);
    let terminal = match ConformantLedger::open(&run_root, run_id) {
        Ok(ledger) => {
            let report = ledger.verify()?;
            report.terminal_sealed
                && matches!(
                    report.lifecycle,
                    RunLifecycle::Closed | RunLifecycle::StartFailed
                )
        }
        Err(ConformanceError::Ledger(LedgerError::Integrity(_)))
        | Err(ConformanceError::InvalidHistory(_)) => {
            terminal_state_escape(&run_root, run_id, escape_snapshot?)?
        }
        Err(error) => return Err(error),
    };
    if !terminal {
        return Err(ConformanceError::DeleteNotTerminal);
    }
    if secure_directory_identity(state_root, uid)? != state_identity
        || secure_directory_identity(&runs_root, uid)? != runs_identity
        || secure_directory_identity(&run_root, uid)? != run_identity
    {
        return Err(ConformanceError::Security(
            "state, runs, or run directory identity changed before deletion".to_owned(),
        ));
    }
    fs::remove_dir_all(&run_root)?;
    sync_directory(&runs_root)?;
    Ok(())
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
    let final_record = read_final_record(&run_root.join("audit.jsonl"))?;
    Ok(TerminalEscapeSnapshot {
        state,
        final_record,
        state_file: file_identity(&state_path)?,
        audit_file: file_identity(&run_root.join("audit.jsonl"))?,
    })
}

fn terminal_state_escape(
    run_root: &Path,
    run_id: Uuid,
    snapshot: TerminalEscapeSnapshot,
) -> Result<bool, ConformanceError> {
    if file_identity(&run_root.join("state.json"))? != snapshot.state_file
        || file_identity(&run_root.join("audit.jsonl"))? != snapshot.audit_file
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
                ledger.inner().durable_records().last().unwrap().payload(),
                "interrupt_terminal_confirmed",
            )
            .unwrap()
        );
        drop(ledger);
        fs::remove_dir_all(root).unwrap();
    }
}
