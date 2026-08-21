use crate::audit::{
    AuditError, AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH, is_microsecond_utc_timestamp,
};
pub use crate::domain::{Access as ProjectedAccess, RunLifecycle};
use crate::fault::{FaultBarrier, FaultInjected, FaultInjector, NoFaults};
use crate::jcs::{
    LosslessJson, RAW_PAYLOAD_LIMIT, REPRESENTED_PAYLOAD_LIMIT, canonicalize, parse, sha256_hex,
};
use crate::workspace::{
    LosslessPath, SystemWorkspacePlatform, WorkspacePlatform, sync_directory,
    verify_secure_directory, verify_secure_file,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const GROUP_COMMIT_MILLIS: u64 = 100;
pub const MAX_EVENT_DELIVERIES: usize = 32;
pub const MAX_EVENT_DELIVERY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_AUDIT_LINE_BYTES: usize = REPRESENTED_PAYLOAD_LIMIT + 64 * 1024;
pub const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendDurability {
    Streaming,
    Required,
}

#[derive(Debug)]
pub enum LedgerError {
    Io(std::io::Error),
    Audit(AuditError),
    Fault(FaultInjected),
    SecurityPolicy(crate::machine::MachineError),
    Integrity(String),
    InvalidRecord(String),
    InvalidEvent(String),
    Projection(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Audit(error) => error.fmt(formatter),
            Self::Fault(error) => error.fmt(formatter),
            Self::SecurityPolicy(error) => {
                write!(
                    formatter,
                    "security policy rejected ledger storage: {}: {}",
                    error.code, error.message
                )
            }
            Self::Integrity(reason) => write!(formatter, "audit integrity failure: {reason}"),
            Self::InvalidRecord(reason) => write!(formatter, "invalid audit record: {reason}"),
            Self::InvalidEvent(reason) => write!(formatter, "invalid client event: {reason}"),
            Self::Projection(reason) => write!(formatter, "invalid state projection: {reason}"),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<std::io::Error> for LedgerError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<AuditError> for LedgerError {
    fn from(value: AuditError) -> Self {
        Self::Audit(value)
    }
}

impl From<FaultInjected> for LedgerError {
    fn from(value: FaultInjected) -> Self {
        Self::Fault(value)
    }
}

pub trait LedgerClock: Send + Sync {
    fn monotonic_millis(&self) -> u64;
    fn timestamp(&self) -> String;
}

#[derive(Clone, Debug)]
pub struct SystemLedgerClock {
    monotonic_origin: Instant,
}

impl Default for SystemLedgerClock {
    fn default() -> Self {
        Self {
            monotonic_origin: Instant::now(),
        }
    }
}

impl LedgerClock for SystemLedgerClock {
    fn monotonic_millis(&self) -> u64 {
        let millis = self.monotonic_origin.elapsed().as_millis();
        u64::try_from(millis).unwrap_or(u64::MAX)
    }

    fn timestamp(&self) -> String {
        format_system_timestamp(SystemTime::now())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerHead {
    pub sequence: u64,
    pub hash: String,
}

impl Default for LedgerHead {
    fn default() -> Self {
        Self {
            sequence: 0,
            hash: GENESIS_PREVIOUS_HASH.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectedWriterAuthority {
    None,
    Reserved,
    Active,
    HandoffPrepared,
    Releasing,
    BlockedUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStateProjection {
    pub schema_version: u32,
    pub run_id: Uuid,
    pub lifecycle: RunLifecycle,
    pub run_generation: u64,
    pub thread_id: Option<String>,
    pub active_turn_id: Option<String>,
    pub latest_turn_id: Option<String>,
    pub pending_requests: Vec<String>,
    pub access: ProjectedAccess,
    pub writer_authority: ProjectedWriterAuthority,
    pub default_effort: Option<String>,
    pub last_event_cursor: Option<String>,
    pub ledger_head: LedgerHead,
}

impl RunStateProjection {
    fn empty(run_id: Uuid) -> Self {
        Self {
            schema_version: 1,
            run_id,
            lifecycle: RunLifecycle::Starting,
            run_generation: 0,
            thread_id: None,
            active_turn_id: None,
            latest_turn_id: None,
            pending_requests: Vec::new(),
            access: ProjectedAccess::Unknown,
            writer_authority: ProjectedWriterAuthority::None,
            default_effort: None,
            last_event_cursor: None,
            ledger_head: LedgerHead::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventProjection {
    Minimal,
    Operational,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStateEventPayload {
    pub previous: Option<String>,
    pub current: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnStateEventPayload {
    pub previous: Option<String>,
    pub current: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FinalResponse {
    Inline { text: String },
    Artifact { artifact: Box<ArtifactMetadata> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMetadata {
    pub schema_version: u32,
    pub artifact_id: Uuid,
    pub run_id: Uuid,
    pub kind: String,
    pub visibility: String,
    pub interaction_request_id: Option<Uuid>,
    pub media_type: String,
    pub byte_length: u64,
    pub sha256: String,
    pub created_at: String,
    pub retention: String,
    pub integrity: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEventPayload {
    pub response: FinalResponse,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionOpenedPayload {
    pub request_id: String,
    pub interaction_kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionResolvedPayload {
    pub request_id: String,
    pub outcome: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeErrorPayload {
    pub error_code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePayload {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceChangesPayload {
    pub paths: Vec<LosslessPath>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterEventPayload {
    pub previous: String,
    pub current: String,
    pub writer_run_id: Option<Uuid>,
    pub writer_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEventPayload {
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandStartedPayload {
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCompletedPayload {
    pub command: Vec<String>,
    pub exit_status: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticPayload {
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationPayload {
    pub run_generation: u64,
    pub server_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningSuppressionPayload {
    pub method: String,
    pub byte_length: u64,
    pub sha256: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum ClientEventData {
    #[serde(rename = "run.state_changed")]
    RunStateChanged(RunStateEventPayload),
    #[serde(rename = "turn.state_changed")]
    TurnStateChanged(TurnStateEventPayload),
    #[serde(rename = "response.final")]
    ResponseFinal(ResponseEventPayload),
    #[serde(rename = "interaction.opened")]
    InteractionOpened(InteractionOpenedPayload),
    #[serde(rename = "interaction.resolved")]
    InteractionResolved(InteractionResolvedPayload),
    #[serde(rename = "runtime.error")]
    RuntimeError(RuntimeErrorPayload),
    #[serde(rename = "usage.reported")]
    UsageReported(UsagePayload),
    #[serde(rename = "workspace.changes")]
    WorkspaceChanges(WorkspaceChangesPayload),
    #[serde(rename = "writer.state_changed")]
    WriterStateChanged(WriterEventPayload),
    #[serde(rename = "recovery.required")]
    RecoveryRequired(RecoveryEventPayload),
    #[serde(rename = "command.started")]
    CommandStarted(CommandStartedPayload),
    #[serde(rename = "command.completed")]
    CommandCompleted(CommandCompletedPayload),
    #[serde(rename = "diagnostic.reported")]
    DiagnosticReported(DiagnosticPayload),
    #[serde(rename = "generation.changed")]
    GenerationChanged(GenerationPayload),
    #[serde(rename = "reasoning.suppressed")]
    ReasoningSuppressed(ReasoningSuppressionPayload),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientEventRecord {
    pub schema_version: u32,
    pub event_schema_version: u32,
    pub cursor: String,
    pub event_id: Uuid,
    pub timestamp: String,
    pub workspace_id: String,
    pub run_id: Uuid,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub server_key: String,
    pub server_epoch: u64,
    #[serde(flatten)]
    pub data: ClientEventData,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventDelivery {
    pub schema_version: u32,
    pub kind: String,
    pub projection: EventProjection,
    pub replay: bool,
    pub record: ClientEventRecord,
}

struct PendingCommit {
    deadline_millis: u64,
    projection: RunStateProjection,
}

#[derive(Default)]
struct ScheduledState {
    pending: Option<PendingCommit>,
    published: Option<RunStateProjection>,
    error: Option<String>,
    cancelled: bool,
}

struct ScheduledCommit<C, F> {
    state: Mutex<ScheduledState>,
    file: File,
    root: PathBuf,
    state_path: PathBuf,
    clock: Arc<C>,
    faults: Arc<F>,
}

pub struct Ledger<C: LedgerClock = SystemLedgerClock, F: FaultInjector = NoFaults> {
    run_id: Uuid,
    root: PathBuf,
    state_path: PathBuf,
    recovery_path: PathBuf,
    file: File,
    records: Vec<AuditRecord>,
    durable_len: usize,
    projection: RunStateProjection,
    pending_since_millis: Option<u64>,
    poisoned: bool,
    clock: Arc<C>,
    faults: Arc<F>,
    scheduled: Arc<ScheduledCommit<C, F>>,
}

impl Ledger<SystemLedgerClock, NoFaults> {
    pub fn open(root: impl Into<PathBuf>, run_id: Uuid) -> Result<Self, LedgerError> {
        Self::open_with(root, run_id, SystemLedgerClock::default(), NoFaults)
    }
}

impl<C: LedgerClock, F: FaultInjector> Drop for Ledger<C, F> {
    fn drop(&mut self) {
        let mut state = self
            .scheduled
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.cancelled = true;
        let Some(pending) = state.pending.take() else {
            return;
        };
        let result = commit_projection(
            &self.file,
            &self.root,
            &self.state_path,
            &*self.faults,
            &pending.projection,
        );
        match result {
            Ok(()) => state.published = Some(pending.projection),
            Err(error) => state.error = Some(error.to_string()),
        }
    }
}

impl<C: LedgerClock + 'static, F: FaultInjector + 'static> Ledger<C, F> {
    pub fn open_with(
        root: impl Into<PathBuf>,
        run_id: Uuid,
        clock: C,
        faults: F,
    ) -> Result<Self, LedgerError> {
        if run_id.get_version_num() != 7 {
            return Err(LedgerError::InvalidRecord(
                "run identity must be UUIDv7".to_owned(),
            ));
        }
        let root = root.into();
        let audit_path = root.join("audit.jsonl");
        let state_path = root.join("state.json");
        let recovery_path = root.join("recovery");
        let uid = current_uid();
        verify_secure_directory(&root, uid).map_err(machine_as_security)?;
        verify_secure_directory(&recovery_path, uid).map_err(machine_as_security)?;
        verify_secure_file(&audit_path, uid).map_err(machine_as_security)?;
        if fs::symlink_metadata(&state_path).is_ok() {
            verify_secure_file(&state_path, uid).map_err(machine_as_security)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&audit_path)?;
        let (mut records, tail) = scan_ledger(&mut file, run_id)?;
        let clock = Arc::new(clock);
        let faults = Arc::new(faults);
        let scheduled = Arc::new(ScheduledCommit {
            state: Mutex::new(ScheduledState::default()),
            file: file.try_clone()?,
            root: root.clone(),
            state_path: state_path.clone(),
            clock: Arc::clone(&clock),
            faults: Arc::clone(&faults),
        });
        let mut ledger = Self {
            run_id,
            root,
            state_path,
            recovery_path,
            file,
            projection: replay(run_id, &records)?,
            durable_len: records.len(),
            records: std::mem::take(&mut records),
            pending_since_millis: None,
            poisoned: false,
            clock,
            faults,
            scheduled,
        };
        if let Some(tail) = tail {
            ledger.preserve_and_truncate_tail(tail)?;
        }
        ledger.finish_pending_repairs()?;
        ledger.reconcile_projection()?;
        Ok(ledger)
    }

    #[must_use]
    pub fn head(&self) -> LedgerHead {
        self.effective_projection().ledger_head
    }

    #[must_use]
    pub fn next_sequence(&self) -> u64 {
        self.records
            .last()
            .map_or(1, |record| record.sequence().saturating_add(1))
    }

    #[must_use]
    pub fn previous_hash(&self) -> &str {
        self.records
            .last()
            .map_or(GENESIS_PREVIOUS_HASH, AuditRecord::hash)
    }

    #[must_use]
    pub fn projection(&self) -> RunStateProjection {
        self.effective_projection()
    }

    #[must_use]
    pub fn durable_records(&self) -> &[AuditRecord] {
        &self.records[..self.effective_durable_len()]
    }

    pub fn append(
        &mut self,
        record: AuditRecord,
        durability: AppendDurability,
    ) -> Result<(), LedgerError> {
        self.refresh_scheduled()?;
        if self.poisoned {
            return Err(LedgerError::Integrity(
                "ledger requires reopen after an ambiguous append or sync failure".to_owned(),
            ));
        }
        self.validate_next(&record)?;
        let mut candidate_records = Vec::with_capacity(self.records.len().saturating_add(1));
        candidate_records.extend_from_slice(&self.records);
        candidate_records.push(record.clone());
        let candidate_projection = replay(self.run_id, &candidate_records).map_err(|error| {
            LedgerError::InvalidRecord(format!("record cannot be projected: {error}"))
        })?;
        let scheduled = Arc::clone(&self.scheduled);
        let mut scheduled_state = scheduled.state.lock().map_err(|_| {
            LedgerError::Integrity("group-commit state lock was poisoned".to_owned())
        })?;
        if let Some(error) = &scheduled_state.error {
            self.poisoned = true;
            return Err(LedgerError::Integrity(error.clone()));
        }
        self.faults.check(FaultBarrier::BeforeLedgerAppend)?;
        let write_result = (|| {
            let line = record.canonical_line()?;
            if line.len() > MAX_AUDIT_LINE_BYTES {
                return Err(LedgerError::InvalidRecord(
                    "canonical audit line exceeds the bounded record limit".to_owned(),
                ));
            }
            self.file.write_all(&line)?;
            self.faults.check(FaultBarrier::AfterLedgerAppend)?;
            Ok::<(), LedgerError>(())
        })();
        if let Err(error) = write_result {
            self.poisoned = true;
            scheduled_state.error = Some(error.to_string());
            return Err(error);
        }
        self.records.push(record);
        let started = *self
            .pending_since_millis
            .get_or_insert_with(|| self.clock.monotonic_millis());
        let mut spawn_scheduler = false;
        if durability == AppendDurability::Streaming {
            let deadline = scheduled_state
                .pending
                .as_ref()
                .map_or(started.saturating_add(GROUP_COMMIT_MILLIS), |pending| {
                    pending.deadline_millis
                });
            spawn_scheduler = scheduled_state.pending.is_none();
            scheduled_state.pending = Some(PendingCommit {
                deadline_millis: deadline,
                projection: candidate_projection,
            });
        }
        drop(scheduled_state);
        if spawn_scheduler {
            spawn_group_commit(Arc::clone(&scheduled));
        }
        match durability {
            AppendDurability::Required => self.flush(),
            AppendDurability::Streaming => Ok(()),
        }
    }

    pub fn append_before_effect<T>(
        &mut self,
        record: AuditRecord,
        effect: impl FnOnce() -> Result<T, LedgerError>,
    ) -> Result<T, LedgerError> {
        self.append(record, AppendDurability::Required)?;
        self.faults.check(FaultBarrier::BeforeExternalEffect)?;
        let result = effect()?;
        self.faults.check(FaultBarrier::AfterExternalEffect)?;
        Ok(result)
    }

    pub fn append_client_event(
        &mut self,
        record: ClientEventRecord,
        run_generation: u64,
        durability: AppendDurability,
    ) -> Result<(), LedgerError> {
        record.validate(self.run_id, self.next_sequence())?;
        let timestamp = record.timestamp.clone();
        let raw = serde_json::to_vec(&record)
            .map_err(|error| LedgerError::InvalidEvent(error.to_string()))?;
        if raw.len() > RAW_PAYLOAD_LIMIT {
            return Err(LedgerError::InvalidEvent(
                "serialized client event exceeds the 2 MiB append limit".to_owned(),
            ));
        }
        let audit = AuditRecord::new_represented(
            self.next_sequence(),
            timestamp,
            self.run_id,
            run_generation,
            AuditKind::ClientEvent,
            &raw,
            self.previous_hash(),
        )?;
        if audit.kind() != AuditKind::ClientEvent {
            return Err(LedgerError::InvalidEvent(
                "client event cannot be represented without changing its audit kind".to_owned(),
            ));
        }
        self.append(audit, durability)
    }

    pub fn append_app_server_message(
        &mut self,
        source_kind: AuditKind,
        method: &str,
        raw: &[u8],
        run_generation: u64,
        durability: AppendDurability,
    ) -> Result<AuditKind, LedgerError> {
        if !bounded(method, 256) {
            return Err(LedgerError::InvalidRecord(
                "app-server method is empty or exceeds 256 UTF-8 bytes".to_owned(),
            ));
        }
        if !matches!(
            source_kind,
            AuditKind::AppServerRequest
                | AuditKind::AppServerResponse
                | AuditKind::AppServerNotification
        ) {
            return Err(LedgerError::InvalidRecord(
                "app-server ingestion requires an app-server audit kind".to_owned(),
            ));
        }
        if is_reasoning_message(method, raw) {
            let payload = ReasoningSuppressionPayload {
                method: method.to_owned(),
                byte_length: u64::try_from(raw.len()).unwrap_or(u64::MAX),
                sha256: sha256_hex(raw),
                reason: "reasoning_content_not_retained".to_owned(),
            };
            let value = serde_json::to_string(&payload)
                .map_err(|error| LedgerError::InvalidRecord(error.to_string()))?;
            let audit = AuditRecord::new(
                self.next_sequence(),
                self.clock.timestamp(),
                self.run_id,
                run_generation,
                AuditKind::ReasoningContentSuppressed,
                parse(&value).map_err(|error| LedgerError::InvalidRecord(error.to_string()))?,
                self.previous_hash(),
            )?;
            self.append(audit, durability)?;
            return Ok(AuditKind::ReasoningContentSuppressed);
        }
        let audit = AuditRecord::new_represented(
            self.next_sequence(),
            self.clock.timestamp(),
            self.run_id,
            run_generation,
            source_kind,
            raw,
            self.previous_hash(),
        )?;
        let stored_kind = audit.kind();
        self.append(audit, durability)?;
        Ok(stored_kind)
    }

    pub fn tick(&mut self) -> Result<bool, LedgerError> {
        self.refresh_scheduled()?;
        if self.group_commit_due() {
            self.flush()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn flush(&mut self) -> Result<(), LedgerError> {
        self.refresh_scheduled()?;
        if self.poisoned {
            return Err(LedgerError::Integrity(
                "ledger requires reopen after an ambiguous append or sync failure".to_owned(),
            ));
        }
        if self.effective_durable_len() == self.records.len() {
            self.durable_len = self.records.len();
            self.pending_since_millis = None;
            return Ok(());
        }
        let scheduled = Arc::clone(&self.scheduled);
        let mut scheduled_state = scheduled.state.lock().map_err(|_| {
            LedgerError::Integrity("group-commit state lock was poisoned".to_owned())
        })?;
        let projection = match replay(self.run_id, &self.records) {
            Ok(projection) => projection,
            Err(error) => {
                self.poisoned = true;
                scheduled_state.error = Some(error.to_string());
                return Err(error);
            }
        };
        if let Err(error) = commit_projection(
            &self.file,
            &self.root,
            &self.state_path,
            &*self.faults,
            &projection,
        ) {
            self.poisoned = true;
            scheduled_state.error = Some(error.to_string());
            return Err(error);
        }
        self.durable_len = self.records.len();
        self.pending_since_millis = None;
        scheduled_state.pending = None;
        scheduled_state.published = Some(projection.clone());
        self.projection = projection;
        Ok(())
    }

    pub fn events_after(
        &self,
        after: u64,
        projection: EventProjection,
        replay_delivery: bool,
    ) -> Result<Vec<EventDelivery>, LedgerError> {
        if after > self.effective_projection().ledger_head.sequence {
            return Err(LedgerError::InvalidEvent(
                "event cursor is beyond the durable ledger head".to_owned(),
            ));
        }
        let mut deliveries = Vec::new();
        let mut delivery_bytes = 0_usize;
        for record in self.durable_records() {
            if record.sequence() <= after || record.kind() != AuditKind::ClientEvent {
                continue;
            }
            let event = event_from_payload(record.payload())?;
            if projection == EventProjection::Minimal && !event.data.minimal() {
                continue;
            }
            let delivery = EventDelivery {
                schema_version: 1,
                kind: "event".to_owned(),
                projection: projection.clone(),
                replay: replay_delivery,
                record: event,
            };
            let encoded_length = serde_json::to_vec(&delivery)
                .map_err(|error| LedgerError::InvalidEvent(error.to_string()))?
                .len();
            if !deliveries.is_empty()
                && (deliveries.len() == MAX_EVENT_DELIVERIES
                    || delivery_bytes.saturating_add(encoded_length) > MAX_EVENT_DELIVERY_BYTES)
            {
                break;
            }
            delivery_bytes = delivery_bytes.saturating_add(encoded_length);
            deliveries.push(delivery);
        }
        Ok(deliveries)
    }

    fn validate_next(&self, record: &AuditRecord) -> Result<(), LedgerError> {
        if record.run_id() != self.run_id {
            return Err(LedgerError::InvalidRecord(
                "record run identity differs from the ledger".to_owned(),
            ));
        }
        if record.sequence() != self.next_sequence() {
            return Err(LedgerError::InvalidRecord(format!(
                "expected sequence {}, received {}",
                self.next_sequence(),
                record.sequence()
            )));
        }
        if record.previous_hash() != self.previous_hash() {
            return Err(LedgerError::InvalidRecord(
                "record previous hash differs from the durable chain".to_owned(),
            ));
        }
        if self
            .records
            .last()
            .is_some_and(|prior| record.run_generation() < prior.run_generation())
        {
            return Err(LedgerError::InvalidRecord(
                "run generation regressed".to_owned(),
            ));
        }
        Ok(())
    }

    fn group_commit_due(&self) -> bool {
        self.pending_since_millis.is_some_and(|started| {
            self.clock.monotonic_millis().saturating_sub(started) >= GROUP_COMMIT_MILLIS
        })
    }

    fn refresh_scheduled(&mut self) -> Result<(), LedgerError> {
        let state = self.scheduled.state.lock().map_err(|_| {
            LedgerError::Integrity("group-commit state lock was poisoned".to_owned())
        })?;
        if let Some(error) = &state.error {
            self.poisoned = true;
            return Err(LedgerError::Integrity(error.clone()));
        }
        if let Some(projection) = &state.published {
            let published_len = usize::try_from(projection.ledger_head.sequence)
                .unwrap_or(usize::MAX)
                .min(self.records.len());
            if published_len > self.durable_len {
                self.durable_len = published_len;
                self.projection = projection.clone();
                if published_len == self.records.len() {
                    self.pending_since_millis = None;
                }
            }
        }
        Ok(())
    }

    fn effective_durable_len(&self) -> usize {
        self.scheduled
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .published
                    .as_ref()
                    .map(|value| value.ledger_head.sequence)
            })
            .and_then(|sequence| usize::try_from(sequence).ok())
            .unwrap_or(self.durable_len)
            .max(self.durable_len)
            .min(self.records.len())
    }

    fn effective_projection(&self) -> RunStateProjection {
        self.scheduled
            .state
            .lock()
            .ok()
            .and_then(|state| state.published.clone())
            .filter(|projection| {
                projection.ledger_head.sequence > self.projection.ledger_head.sequence
            })
            .unwrap_or_else(|| self.projection.clone())
    }

    fn preserve_and_truncate_tail(&mut self, tail: TornTail) -> Result<(), LedgerError> {
        let sequence = self.next_sequence();
        let digest = sha256_hex(&tail.bytes);
        let filename = format!("tail-{sequence}-{digest}.bin");
        let evidence = self.recovery_path.join(&filename);
        if fs::symlink_metadata(&evidence).is_ok() {
            verify_secure_file(&evidence, current_uid()).map_err(machine_as_security)?;
            let (file, bytes) = open_bounded_recovery_evidence(&evidence)?;
            if bytes != tail.bytes {
                return Err(LedgerError::Integrity(format!(
                    "existing recovery evidence {} differs from the torn tail",
                    evidence.display()
                )));
            }
            self.faults
                .check(FaultBarrier::BeforeTailEvidenceFileSync)?;
            file.sync_all()?;
            self.faults.check(FaultBarrier::AfterTailEvidenceFileSync)?;
        } else {
            let temporary = self
                .recovery_path
                .join(format!(".tail-evidence-{}.tmp", Uuid::now_v7()));
            let publish_result = (|| {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temporary)?;
                file.write_all(&tail.bytes)?;
                self.faults
                    .check(FaultBarrier::BeforeTailEvidenceFileSync)?;
                file.sync_all()?;
                self.faults.check(FaultBarrier::AfterTailEvidenceFileSync)?;
                match fs::hard_link(&temporary, &evidence) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        verify_secure_file(&evidence, current_uid())
                            .map_err(machine_as_security)?;
                        let (existing, bytes) = open_bounded_recovery_evidence(&evidence)?;
                        if bytes != tail.bytes {
                            Err(LedgerError::Integrity(format!(
                                "existing recovery evidence {} differs from the torn tail",
                                evidence.display()
                            )))
                        } else {
                            existing.sync_all()?;
                            Ok(())
                        }
                    }
                    Err(error) => Err(error.into()),
                }
            })();
            let cleanup_result = fs::remove_file(&temporary);
            publish_result?;
            if let Err(error) = cleanup_result
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error.into());
            }
        }
        self.faults
            .check(FaultBarrier::BeforeTailEvidenceDirectorySync)?;
        sync_directory(&self.recovery_path)?;
        self.faults
            .check(FaultBarrier::AfterTailEvidenceDirectorySync)?;
        self.faults.check(FaultBarrier::BeforeLedgerTruncate)?;
        self.file.set_len(tail.prefix_length)?;
        self.file.seek(SeekFrom::End(0))?;
        self.faults.check(FaultBarrier::AfterLedgerTruncate)?;
        self.faults.check(FaultBarrier::BeforeLedgerFileSync)?;
        self.file.sync_all()?;
        self.faults.check(FaultBarrier::AfterLedgerFileSync)?;
        Ok(())
    }

    fn finish_pending_repairs(&mut self) -> Result<(), LedgerError> {
        let recorded = self
            .records
            .iter()
            .filter(|record| record.kind() == AuditKind::LedgerTailRepaired)
            .filter_map(|record| payload_string(record.payload(), "evidence_file"))
            .collect::<BTreeSet<_>>();
        let mut pending = Vec::new();
        let mut removed_temporary = false;
        for entry in fs::read_dir(&self.recovery_path)? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| {
                LedgerError::Integrity("recovery evidence name is not valid UTF-8".to_owned())
            })?;
            if name.starts_with(".tail-evidence-") && name.ends_with(".tmp") {
                verify_secure_file(&entry.path(), current_uid()).map_err(machine_as_security)?;
                fs::remove_file(entry.path())?;
                removed_temporary = true;
                continue;
            }
            let Some((sequence, digest)) = parse_tail_evidence_name(&name) else {
                return Err(LedgerError::Integrity(format!(
                    "unrecognized recovery entry: {name}"
                )));
            };
            verify_secure_file(&entry.path(), current_uid()).map_err(machine_as_security)?;
            let (_, bytes) = open_bounded_recovery_evidence(&entry.path())?;
            if sha256_hex(&bytes) != digest {
                return Err(LedgerError::Integrity(format!(
                    "recovery evidence digest mismatch: {name}"
                )));
            }
            if !recorded.contains(&name) {
                pending.push((sequence, name, digest, bytes.len()));
            }
        }
        if removed_temporary {
            sync_directory(&self.recovery_path)?;
        }
        pending.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        for (sequence, filename, digest, length) in pending {
            if sequence > self.next_sequence() {
                return Err(LedgerError::Integrity(format!(
                    "unrecorded recovery evidence {filename} names future sequence {sequence}, current next sequence is {}",
                    self.next_sequence()
                )));
            }
            let payload = LosslessJson::Object(vec![
                ("evidence_file".to_owned(), LosslessJson::String(filename)),
                ("raw_sha256".to_owned(), LosslessJson::String(digest)),
                (
                    "truncated_byte_length".to_owned(),
                    LosslessJson::Number(length.to_string()),
                ),
            ]);
            let record = AuditRecord::new(
                self.next_sequence(),
                self.clock.timestamp(),
                self.run_id,
                self.records.last().map_or(0, AuditRecord::run_generation),
                AuditKind::LedgerTailRepaired,
                payload,
                self.previous_hash(),
            )?;
            self.append(record, AppendDurability::Required)?;
        }
        Ok(())
    }

    fn reconcile_projection(&mut self) -> Result<(), LedgerError> {
        let rebuilt = replay(self.run_id, self.durable_records())?;
        let existing = match read_bounded(&self.state_path, MAX_STATE_BYTES) {
            Ok(bytes) => serde_json::from_slice::<RunStateProjection>(&bytes).ok(),
            Err(LedgerError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let ahead = existing.as_ref().is_some_and(|state| {
            state.schema_version == 1
                && state.run_id == self.run_id
                && state.ledger_head.sequence > rebuilt.ledger_head.sequence
        });
        if ahead {
            let old = existing.as_ref().expect("ahead projection exists");
            let payload = LosslessJson::Object(vec![
                (
                    "projected_sequence".to_owned(),
                    LosslessJson::Number(old.ledger_head.sequence.to_string()),
                ),
                (
                    "projected_hash".to_owned(),
                    LosslessJson::String(old.ledger_head.hash.clone()),
                ),
                (
                    "durable_sequence".to_owned(),
                    LosslessJson::Number(rebuilt.ledger_head.sequence.to_string()),
                ),
                (
                    "durable_hash".to_owned(),
                    LosslessJson::String(rebuilt.ledger_head.hash.clone()),
                ),
            ]);
            let record = AuditRecord::new(
                self.next_sequence(),
                self.clock.timestamp(),
                self.run_id,
                self.records.last().map_or(0, AuditRecord::run_generation),
                AuditKind::ProjectionRewound,
                payload,
                self.previous_hash(),
            )?;
            self.append(record, AppendDurability::Required)?;
            return Ok(());
        }
        let current = existing.as_ref().is_some_and(|state| state == &rebuilt);
        if !current {
            self.publish_projection(&rebuilt)?;
        }
        self.projection = rebuilt;
        Ok(())
    }

    fn publish_projection(&self, projection: &RunStateProjection) -> Result<(), LedgerError> {
        publish_projection_to(
            &self.root,
            &self.state_path,
            projection,
            &*self.faults,
            self.durable_len as u64,
        )
    }
}

fn spawn_group_commit<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    scheduled: Arc<ScheduledCommit<C, F>>,
) {
    thread::spawn(move || {
        loop {
            let deadline = match scheduled.state.lock() {
                Ok(state) if state.cancelled => return,
                Ok(state) => match &state.pending {
                    Some(pending) => pending.deadline_millis,
                    None => return,
                },
                Err(_) => return,
            };
            let remaining = deadline.saturating_sub(scheduled.clock.monotonic_millis());
            if remaining > 0 {
                thread::sleep(Duration::from_millis(remaining.min(5)));
                continue;
            }
            let mut state = match scheduled.state.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            if state.cancelled {
                return;
            }
            let Some(pending) = state.pending.take() else {
                return;
            };
            if scheduled.clock.monotonic_millis() < pending.deadline_millis {
                state.pending = Some(pending);
                drop(state);
                continue;
            }
            let result = commit_projection(
                &scheduled.file,
                &scheduled.root,
                &scheduled.state_path,
                &*scheduled.faults,
                &pending.projection,
            );
            match result {
                Ok(()) => state.published = Some(pending.projection),
                Err(error) => {
                    state.error = Some(error.to_string());
                    state.pending = Some(pending);
                }
            }
            return;
        }
    });
}

fn commit_projection(
    file: &File,
    root: &Path,
    state_path: &Path,
    faults: &dyn FaultInjector,
    projection: &RunStateProjection,
) -> Result<(), LedgerError> {
    faults.check(FaultBarrier::BeforeLedgerFileSync)?;
    file.sync_all()?;
    faults.check(FaultBarrier::AfterLedgerFileSync)?;
    publish_projection_to(
        root,
        state_path,
        projection,
        faults,
        projection.ledger_head.sequence,
    )
}

fn publish_projection_to(
    root: &Path,
    state_path: &Path,
    projection: &RunStateProjection,
    faults: &dyn FaultInjector,
    durable_sequence: u64,
) -> Result<(), LedgerError> {
    validate_projection(projection)?;
    if projection.ledger_head.sequence > durable_sequence {
        return Err(LedgerError::Projection(
            "state head exceeds the last fsynced record".to_owned(),
        ));
    }
    faults.check(FaultBarrier::BeforeProjectionReplace)?;
    let serialized = serde_json::to_string(projection)
        .map_err(|error| LedgerError::Projection(error.to_string()))?;
    let parsed = parse(&serialized).map_err(|error| LedgerError::Projection(error.to_string()))?;
    let mut bytes =
        canonicalize(&parsed).map_err(|error| LedgerError::Projection(error.to_string()))?;
    bytes.push(b'\n');
    let temporary = root.join(format!(".state-{}.tmp", Uuid::now_v7()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        faults.check(FaultBarrier::BeforeProjectionFileSync)?;
        file.sync_all()?;
        faults.check(FaultBarrier::AfterProjectionFileSync)?;
        fs::rename(&temporary, state_path)?;
        faults.check(FaultBarrier::BeforeProjectionDirectorySync)?;
        sync_directory(root)?;
        faults.check(FaultBarrier::AfterProjectionDirectorySync)?;
        Ok::<(), LedgerError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Debug)]
struct TornTail {
    prefix_length: u64,
    bytes: Vec<u8>,
}

fn scan_ledger(
    file: &mut File,
    run_id: Uuid,
) -> Result<(Vec<AuditRecord>, Option<TornTail>), LedgerError> {
    let mut reader = BufReader::new(file.try_clone()?);
    reader.seek(SeekFrom::Start(0))?;
    let mut records = Vec::new();
    let mut prefix_length = 0_u64;
    let mut tail = None;
    while let Some((mut line, terminated)) = read_bounded_segment(&mut reader)? {
        let byte_count = line.len();
        if !terminated {
            tail = Some(TornTail {
                prefix_length,
                bytes: line,
            });
            break;
        }
        line.pop();
        if line.is_empty() {
            return Err(LedgerError::Integrity(format!(
                "empty newline-terminated record at line {}",
                records.len() + 1
            )));
        }
        let index = records.len();
        let record = AuditRecord::from_canonical_line(&line).map_err(|error| {
            LedgerError::Integrity(format!("line {} is invalid: {error}", index + 1))
        })?;
        let expected_sequence = u64::try_from(records.len()).expect("record count fits u64") + 1;
        let expected_previous = records
            .last()
            .map_or(GENESIS_PREVIOUS_HASH, AuditRecord::hash);
        if record.sequence() != expected_sequence {
            return Err(LedgerError::Integrity(format!(
                "line {} has sequence {}, expected {expected_sequence}",
                index + 1,
                record.sequence()
            )));
        }
        if record.previous_hash() != expected_previous {
            return Err(LedgerError::Integrity(format!(
                "line {} breaks the hash chain",
                index + 1
            )));
        }
        if record.run_id() != run_id {
            return Err(LedgerError::Integrity(format!(
                "line {} belongs to another run",
                index + 1
            )));
        }
        if records
            .last()
            .is_some_and(|prior: &AuditRecord| record.run_generation() < prior.run_generation())
        {
            return Err(LedgerError::Integrity(format!(
                "line {} regresses run generation",
                index + 1
            )));
        }
        records.push(record);
        prefix_length = prefix_length.saturating_add(u64::try_from(byte_count).unwrap_or(u64::MAX));
    }
    file.seek(SeekFrom::End(0))?;
    Ok((records, tail))
}

fn read_bounded_segment(reader: &mut impl BufRead) -> Result<Option<(Vec<u8>, bool)>, LedgerError> {
    let mut output = Vec::with_capacity(8192);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                Ok(Some((output, false)))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if output.len().saturating_add(take) > MAX_AUDIT_LINE_BYTES {
            return Err(LedgerError::Integrity(
                "audit line or torn tail exceeds the bounded record limit".to_owned(),
            ));
        }
        let terminated = available.get(take.saturating_sub(1)) == Some(&b'\n');
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if terminated {
            return Ok(Some((output, true)));
        }
    }
}

fn replay(run_id: Uuid, records: &[AuditRecord]) -> Result<RunStateProjection, LedgerError> {
    let mut state = RunStateProjection::empty(run_id);
    let mut pending = BTreeSet::new();
    for record in records {
        if record.run_id() != run_id {
            return Err(LedgerError::Integrity(
                "replay crossed run identity".to_owned(),
            ));
        }
        state.run_generation = state.run_generation.max(record.run_generation());
        match record.kind() {
            AuditKind::RunCreated => {
                state.lifecycle = RunLifecycle::Starting;
                if let Some(access) = payload_string(record.payload(), "initial_access") {
                    state.access = parse_access(&access)?;
                }
                state.default_effort = payload_string(record.payload(), "default_effort");
                if state
                    .default_effort
                    .as_deref()
                    .is_some_and(|value| !bounded(value, 64))
                {
                    return Err(LedgerError::Integrity(
                        "default effort exceeds its projection bound".to_owned(),
                    ));
                }
            }
            AuditKind::ThreadBound => {
                let thread = required_payload_string(record.payload(), "thread_id")?;
                if !bounded(&thread, 256) {
                    return Err(LedgerError::Integrity(
                        "thread identity exceeds its projection bound".to_owned(),
                    ));
                }
                state.thread_id = Some(thread);
            }
            AuditKind::TurnStarted => {
                let turn = required_payload_string(record.payload(), "turn_id")?;
                if !bounded(&turn, 256) {
                    return Err(LedgerError::Integrity(
                        "turn identity exceeds its projection bound".to_owned(),
                    ));
                }
                state.active_turn_id = Some(turn.clone());
                state.latest_turn_id = Some(turn);
                state.lifecycle = RunLifecycle::Running;
            }
            AuditKind::TurnTerminal => {
                let turn = required_payload_string(record.payload(), "turn_id")?;
                if !bounded(&turn, 256) {
                    return Err(LedgerError::Integrity(
                        "turn identity exceeds its projection bound".to_owned(),
                    ));
                }
                if state.active_turn_id.as_deref() == Some(turn.as_str()) {
                    state.active_turn_id = None;
                }
                state.latest_turn_id = Some(turn);
                state.lifecycle = if pending.is_empty() {
                    RunLifecycle::Idle
                } else {
                    RunLifecycle::WaitingInteraction
                };
            }
            AuditKind::LifecycleTransition => {
                state.lifecycle =
                    parse_lifecycle(&required_payload_string(record.payload(), "current")?)?;
                if matches!(
                    state.lifecycle,
                    RunLifecycle::Closed | RunLifecycle::StartFailed | RunLifecycle::OutcomeUnknown
                ) {
                    state.active_turn_id = None;
                }
            }
            AuditKind::RunGenerationStarted | AuditKind::RunGenerationStopped => {
                state.run_generation = record.run_generation();
            }
            AuditKind::InteractionOpened => {
                let request = required_payload_string(record.payload(), "request_id")?;
                if !bounded(&request, 256) || pending.len() >= 4096 {
                    return Err(LedgerError::Integrity(
                        "pending interaction exceeds projection bounds".to_owned(),
                    ));
                }
                pending.insert(request);
                state.lifecycle = RunLifecycle::WaitingInteraction;
            }
            AuditKind::InteractionResolved => {
                let request = required_payload_string(record.payload(), "request_id")?;
                if !bounded(&request, 256) {
                    return Err(LedgerError::Integrity(
                        "resolved interaction exceeds its projection bound".to_owned(),
                    ));
                }
                pending.remove(&request);
                state.lifecycle = if pending.is_empty() {
                    if state.active_turn_id.is_some() {
                        RunLifecycle::Running
                    } else {
                        RunLifecycle::Idle
                    }
                } else {
                    RunLifecycle::WaitingInteraction
                };
            }
            AuditKind::WriterAcquired => {
                state.writer_authority = ProjectedWriterAuthority::Active;
                state.access = ProjectedAccess::Write;
            }
            AuditKind::WriterReleased => {
                state.writer_authority = ProjectedWriterAuthority::None;
                state.access = ProjectedAccess::Read;
            }
            AuditKind::WriterHandoffRequested => {
                state.writer_authority = ProjectedWriterAuthority::HandoffPrepared;
                state.access = ProjectedAccess::Transitioning;
            }
            AuditKind::WriterHandoffCancelled => {
                state.writer_authority = ProjectedWriterAuthority::Active;
                state.access = ProjectedAccess::Write;
            }
            AuditKind::WriterHandoffCompleted => {
                state.writer_authority = ProjectedWriterAuthority::None;
                state.access = ProjectedAccess::Read;
            }
            AuditKind::Reconciliation => {
                state.lifecycle = RunLifecycle::ReconciliationRequired;
            }
            AuditKind::ClientEvent => {
                let event = event_from_payload(record.payload())?;
                event.validate(run_id, record.sequence()).map_err(|error| {
                    LedgerError::Integrity(format!("stored client event is invalid: {error}"))
                })?;
                state.last_event_cursor = Some(event.cursor);
            }
            AuditKind::StartFailed => {
                state.lifecycle = RunLifecycle::StartFailed;
                state.active_turn_id = None;
            }
            AuditKind::OutcomeUnknown => {
                state.lifecycle = RunLifecycle::OutcomeUnknown;
                state.active_turn_id = None;
            }
            _ => {}
        }
        state.ledger_head = LedgerHead {
            sequence: record.sequence(),
            hash: record.hash().to_owned(),
        };
    }
    state.pending_requests = pending.into_iter().collect();
    validate_projection(&state)?;
    Ok(state)
}

impl ClientEventRecord {
    fn validate(&self, run_id: Uuid, sequence: u64) -> Result<(), LedgerError> {
        let invalid = |reason: &str| LedgerError::InvalidEvent(reason.to_owned());
        if self.schema_version != 1 || self.event_schema_version != 1 {
            return Err(invalid("unsupported event schema version"));
        }
        if self.cursor != sequence.to_string() {
            return Err(invalid("event cursor must equal the audit sequence"));
        }
        if self.event_id.get_version_num() != 7
            || self.run_id != run_id
            || run_id.get_version_num() != 7
        {
            return Err(invalid("event or run identity is invalid"));
        }
        if !is_microsecond_utc_timestamp(&self.timestamp)
            || !is_sha256(&self.workspace_id)
            || !is_sha256(&self.server_key)
            || self.server_epoch == 0
        {
            return Err(invalid("event provenance is invalid"));
        }
        if self
            .thread_id
            .as_deref()
            .is_some_and(|value| !bounded(value, 256))
            || self
                .turn_id
                .as_deref()
                .is_some_and(|value| !bounded(value, 256))
        {
            return Err(invalid("event thread or turn identity is invalid"));
        }
        if self.data.requires_turn() && (self.thread_id.is_none() || self.turn_id.is_none()) {
            return Err(invalid(
                "event type requires exact thread and turn identities",
            ));
        }
        self.data.validate(self.run_id)
    }
}

impl ClientEventData {
    fn minimal(&self) -> bool {
        matches!(
            self,
            Self::RunStateChanged(_)
                | Self::TurnStateChanged(_)
                | Self::ResponseFinal(_)
                | Self::InteractionOpened(_)
                | Self::InteractionResolved(_)
                | Self::RuntimeError(_)
                | Self::WriterStateChanged(_)
                | Self::RecoveryRequired(_)
        )
    }

    fn requires_turn(&self) -> bool {
        matches!(
            self,
            Self::TurnStateChanged(_)
                | Self::ResponseFinal(_)
                | Self::InteractionOpened(_)
                | Self::InteractionResolved(_)
                | Self::UsageReported(_)
                | Self::WorkspaceChanges(_)
                | Self::CommandStarted(_)
                | Self::CommandCompleted(_)
                | Self::ReasoningSuppressed(_)
        )
    }

    fn validate(&self, run_id: Uuid) -> Result<(), LedgerError> {
        let invalid = |reason: &str| LedgerError::InvalidEvent(reason.to_owned());
        match self {
            Self::RunStateChanged(payload) => {
                if payload
                    .previous
                    .as_deref()
                    .is_some_and(|value| !valid_run_state(value))
                    || !valid_run_state(&payload.current)
                {
                    return Err(invalid("run state event contains an unknown state"));
                }
            }
            Self::TurnStateChanged(payload) => {
                if payload
                    .previous
                    .as_deref()
                    .is_some_and(|value| !valid_turn_state(value))
                    || !valid_turn_state(&payload.current)
                {
                    return Err(invalid("turn state event contains an unknown state"));
                }
            }
            Self::ResponseFinal(payload) => {
                payload.response.validate(run_id)?;
            }
            Self::InteractionOpened(payload) => {
                if !bounded(&payload.request_id, 256)
                    || !matches!(
                        payload.interaction_kind.as_str(),
                        "command_execution_approval"
                            | "file_change_approval"
                            | "user_input"
                            | "unsupported_request"
                    )
                {
                    return Err(invalid("interaction-opened payload is invalid"));
                }
            }
            Self::InteractionResolved(payload) => {
                if !bounded(&payload.request_id, 256)
                    || !matches!(
                        payload.outcome.as_str(),
                        "accepted"
                            | "declined"
                            | "cancelled"
                            | "answered"
                            | "stale"
                            | "method_not_found"
                    )
                {
                    return Err(invalid("interaction-resolved payload is invalid"));
                }
            }
            Self::RuntimeError(payload) => {
                if payload.error_code.is_empty()
                    || !payload.error_code.bytes().enumerate().all(|(index, byte)| {
                        if index == 0 {
                            byte.is_ascii_uppercase()
                        } else {
                            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                        }
                    })
                    || !bounded(&payload.message, 4096)
                {
                    return Err(invalid("runtime-error payload is invalid"));
                }
            }
            Self::UsageReported(_) => {}
            Self::WorkspaceChanges(payload) => {
                if payload.paths.len() > 4096
                    || payload.paths.iter().any(|path| match path {
                        LosslessPath::Utf8(value) => !bounded(value, 4096),
                        LosslessPath::Bytes { bytes } => {
                            if bytes.len() > 8192 {
                                return true;
                            }
                            let Ok(decoded) =
                                base64::engine::general_purpose::STANDARD.decode(bytes)
                            else {
                                return true;
                            };
                            base64::engine::general_purpose::STANDARD.encode(decoded) != *bytes
                        }
                    })
                {
                    return Err(invalid("workspace-changes payload exceeds bounds"));
                }
            }
            Self::WriterStateChanged(payload) => {
                if !valid_writer_state(&payload.previous)
                    || !valid_writer_state(&payload.current)
                    || payload
                        .writer_run_id
                        .is_some_and(|identity| identity.get_version_num() != 7)
                {
                    return Err(invalid("writer payload is invalid"));
                }
            }
            Self::RecoveryRequired(payload) => {
                if !bounded(&payload.reason, 4096) {
                    return Err(invalid("recovery reason exceeds its bound"));
                }
            }
            Self::CommandStarted(payload) => validate_command(&payload.command)?,
            Self::CommandCompleted(payload) => validate_command(&payload.command)?,
            Self::DiagnosticReported(payload) => {
                if !bounded(&payload.message, 4096) {
                    return Err(invalid("diagnostic message exceeds its bound"));
                }
            }
            Self::GenerationChanged(payload) => {
                if payload.server_epoch == 0 {
                    return Err(invalid("generation event requires a positive server epoch"));
                }
            }
            Self::ReasoningSuppressed(payload) => {
                if !bounded(&payload.method, 256)
                    || !is_sha256(&payload.sha256)
                    || payload.reason != "reasoning_content_not_retained"
                {
                    return Err(invalid("reasoning suppression metadata is invalid"));
                }
            }
        }
        Ok(())
    }
}

impl FinalResponse {
    fn validate(&self, run_id: Uuid) -> Result<(), LedgerError> {
        let invalid = |reason: &str| LedgerError::InvalidEvent(reason.to_owned());
        match self {
            Self::Inline { text } => {
                if !bounded(text, 1024 * 1024) {
                    return Err(invalid("inline final response exceeds 1 MiB"));
                }
            }
            Self::Artifact { artifact } => artifact.validate(run_id)?,
        }
        Ok(())
    }
}

impl ArtifactMetadata {
    fn validate(&self, run_id: Uuid) -> Result<(), LedgerError> {
        let invalid = |reason: &str| LedgerError::InvalidEvent(reason.to_owned());
        if self.schema_version != 1
            || self.artifact_id.get_version_num() != 7
            || self.run_id != run_id
            || self.kind != "final_response"
            || self.visibility != "observer"
            || self.interaction_request_id.is_some()
            || self.media_type != "text/markdown"
            || self.byte_length > 32 * 1024 * 1024
            || !is_sha256(&self.sha256)
            || !is_microsecond_utc_timestamp(&self.created_at)
            || self.retention != "run_lifetime"
            || !matches!(
                self.integrity.as_str(),
                "verified" | "unverified" | "failed"
            )
        {
            return Err(invalid("final-response artifact metadata is invalid"));
        }
        Ok(())
    }
}

fn event_from_payload(payload: &LosslessJson) -> Result<ClientEventRecord, LedgerError> {
    const EVENT_FIELDS: [&str; 13] = [
        "schema_version",
        "event_schema_version",
        "cursor",
        "event_id",
        "timestamp",
        "workspace_id",
        "run_id",
        "thread_id",
        "turn_id",
        "server_key",
        "server_epoch",
        "type",
        "payload",
    ];
    let LosslessJson::Object(entries) = payload else {
        return Err(LedgerError::Integrity(
            "client event payload is not an object".to_owned(),
        ));
    };
    if entries.len() != EVENT_FIELDS.len()
        || entries
            .iter()
            .any(|(name, _)| !EVENT_FIELDS.contains(&name.as_str()))
    {
        return Err(LedgerError::Integrity(
            "client event has missing or unknown top-level fields".to_owned(),
        ));
    }
    let bytes = canonicalize(payload).map_err(|error| {
        LedgerError::Integrity(format!("client event is not canonical: {error}"))
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| LedgerError::Integrity(format!("client event is invalid: {error}")))
}

fn is_reasoning_message(method: &str, raw: &[u8]) -> bool {
    if method.starts_with("item/reasoning/") {
        return true;
    }
    if !matches!(method, "item/started" | "item/completed") {
        return false;
    }
    let Ok(text) = std::str::from_utf8(raw) else {
        return false;
    };
    let Ok(value) = parse(text) else {
        return false;
    };
    lossless_path_string(&value, &["params", "item", "type"]).as_deref() == Some("reasoning")
}

fn lossless_path_string(value: &LosslessJson, path: &[&str]) -> Option<String> {
    let mut current = value;
    for component in path {
        let LosslessJson::Object(entries) = current else {
            return None;
        };
        current = entries
            .iter()
            .find_map(|(key, value)| (key == component).then_some(value))?;
    }
    match current {
        LosslessJson::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn payload_string(payload: &LosslessJson, name: &str) -> Option<String> {
    let LosslessJson::Object(entries) = payload else {
        return None;
    };
    entries.iter().find_map(|(key, value)| {
        (key == name)
            .then_some(value)
            .and_then(|value| match value {
                LosslessJson::String(value) => Some(value.clone()),
                _ => None,
            })
    })
}

fn required_payload_string(payload: &LosslessJson, name: &str) -> Result<String, LedgerError> {
    payload_string(payload, name).ok_or_else(|| {
        LedgerError::Integrity(format!("audit payload is missing required string {name}"))
    })
}

fn parse_lifecycle(value: &str) -> Result<RunLifecycle, LedgerError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| LedgerError::Integrity(format!("unknown lifecycle projection {value}")))
}

fn parse_access(value: &str) -> Result<ProjectedAccess, LedgerError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| LedgerError::Integrity(format!("unknown access projection {value}")))
}

fn valid_run_state(value: &str) -> bool {
    parse_lifecycle(value).is_ok()
}

fn valid_turn_state(value: &str) -> bool {
    matches!(
        value,
        "reserved"
            | "accepted"
            | "running"
            | "waiting_interaction"
            | "interrupting"
            | "completed"
            | "failed"
            | "interrupted"
            | "outcome_unknown"
    )
}

fn valid_writer_state(value: &str) -> bool {
    matches!(
        value,
        "none" | "reserved" | "active" | "handoff_prepared" | "releasing" | "blocked_unknown"
    )
}

fn validate_command(command: &[String]) -> Result<(), LedgerError> {
    if command.len() > 256 || command.iter().any(|argument| !bounded(argument, 4096)) {
        return Err(LedgerError::InvalidEvent(
            "command payload exceeds its bounds".to_owned(),
        ));
    }
    Ok(())
}

fn validate_projection(projection: &RunStateProjection) -> Result<(), LedgerError> {
    let invalid = |reason: &str| LedgerError::Projection(reason.to_owned());
    if projection.schema_version != 1 || projection.run_id.get_version_num() != 7 {
        return Err(invalid("projection schema or run identity is invalid"));
    }
    if !is_sha256(&projection.ledger_head.hash)
        || (projection.ledger_head.sequence == 0
            && projection.ledger_head.hash != GENESIS_PREVIOUS_HASH)
    {
        return Err(invalid("projection ledger head is invalid"));
    }
    if projection
        .thread_id
        .as_deref()
        .is_some_and(|value| !bounded(value, 256))
        || projection
            .active_turn_id
            .as_deref()
            .is_some_and(|value| !bounded(value, 256))
        || projection
            .latest_turn_id
            .as_deref()
            .is_some_and(|value| !bounded(value, 256))
        || projection
            .default_effort
            .as_deref()
            .is_some_and(|value| !bounded(value, 64))
    {
        return Err(invalid("projection identity or effort exceeds its bound"));
    }
    if projection.pending_requests.len() > 4096
        || projection
            .pending_requests
            .iter()
            .any(|value| !bounded(value, 256))
        || projection
            .pending_requests
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(invalid(
            "pending requests are not bounded, sorted, and unique",
        ));
    }
    if projection.active_turn_id.is_some() && projection.active_turn_id != projection.latest_turn_id
    {
        return Err(invalid("active turn is not the latest turn"));
    }
    if matches!(
        projection.lifecycle,
        RunLifecycle::Closed | RunLifecycle::StartFailed | RunLifecycle::OutcomeUnknown
    ) && projection.active_turn_id.is_some()
    {
        return Err(invalid("terminal projection retains an active turn"));
    }
    if projection.lifecycle == RunLifecycle::WaitingInteraction
        && projection.pending_requests.is_empty()
    {
        return Err(invalid("waiting projection has no pending interaction"));
    }
    if projection.writer_authority == ProjectedWriterAuthority::Active
        && projection.access != ProjectedAccess::Write
    {
        return Err(invalid("active writer projection is not write-capable"));
    }
    if projection
        .last_event_cursor
        .as_deref()
        .is_some_and(|cursor| {
            cursor.parse::<u64>().map_or(true, |value| {
                value == 0 || value > projection.ledger_head.sequence || cursor.starts_with('0')
            })
        })
    {
        return Err(invalid(
            "event cursor exceeds or disagrees with the ledger head",
        ));
    }
    Ok(())
}

fn read_bounded(path: &PathBuf, maximum: u64) -> Result<Vec<u8>, LedgerError> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > maximum {
        return Err(LedgerError::Projection(format!(
            "{} exceeds the bounded projection size",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(LedgerError::Projection(format!(
            "{} exceeds the bounded projection size",
            path.display()
        )));
    }
    Ok(bytes)
}

fn open_bounded_recovery_evidence(path: &Path) -> Result<(File, Vec<u8>), LedgerError> {
    let file = OpenOptions::new().read(true).open(path)?;
    let metadata = file.metadata()?;
    let maximum = u64::try_from(MAX_AUDIT_LINE_BYTES).unwrap_or(u64::MAX);
    if metadata.len() > maximum {
        return Err(LedgerError::Integrity(format!(
            "recovery evidence {} exceeds the bounded evidence size",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.try_clone()?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(LedgerError::Integrity(format!(
            "recovery evidence {} exceeds the bounded evidence size",
            path.display()
        )));
    }
    Ok((file, bytes))
}

fn bounded(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_tail_evidence_name(name: &str) -> Option<(u64, String)> {
    let rest = name.strip_prefix("tail-")?.strip_suffix(".bin")?;
    let (sequence, digest) = rest.split_once('-')?;
    let sequence = sequence.parse().ok()?;
    is_sha256(digest).then(|| (sequence, digest.to_owned()))
}

fn machine_as_security(error: crate::machine::MachineError) -> LedgerError {
    LedgerError::SecurityPolicy(error)
}

fn current_uid() -> u32 {
    SystemWorkspacePlatform.current_uid()
}

fn format_system_timestamp(time: SystemTime) -> String {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let seconds = duration.as_secs();
    let micros = duration.subsec_micros();
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micros:06}Z")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let days = days_since_epoch + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}
