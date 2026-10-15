pub use crate::audit::AuditRecord;
use crate::audit::{AuditError, AuditKind, GENESIS_PREVIOUS_HASH};
use crate::darwin::DarwinSystem;
use crate::fault::{FaultBarrier, FaultInjected, FaultInjector, NoFaults};
use crate::jcs::{
    LosslessJson, RAW_PAYLOAD_LIMIT, REPRESENTED_PAYLOAD_LIMIT, canonicalize, parse, sha256_hex,
};
pub use crate::projection::{
    LedgerHead, ProjectedAccess, ProjectedWriterAuthority, RunLifecycle, RunStateProjection,
};
use crate::workspace::{
    SystemWorkspacePlatform, WorkspacePlatform, sync_directory, verify_secure_directory,
    verify_secure_file,
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const GROUP_COMMIT_MILLIS: u64 = 100;
pub const MAX_EVENT_DELIVERIES: usize = 32;
pub const MAX_EVENT_DELIVERY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_AUDIT_LINE_BYTES: usize = REPRESENTED_PAYLOAD_LIMIT + 64 * 1024;
pub const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_LEDGER_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_LEDGER_RECORDS: usize = 1_000_000;
pub const FINALIZATION_RESERVED_BYTES: u64 = 32 * MAX_AUDIT_LINE_BYTES as u64;
pub const FINALIZATION_RESERVED_RECORDS: usize = 256;
pub const REPLAY_BUDGET_MILLIS: u64 = 5 * 60 * 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendDurability {
    Streaming,
    Required,
}

#[derive(Debug)]
pub enum LedgerError {
    Io(std::io::Error),
    IoContext {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Audit(AuditError),
    Fault(FaultInjected),
    SecurityPolicy(crate::machine::MachineError),
    Integrity(String),
    CaptureConflict,
    Capacity(String),
    InvalidRecord(String),
    InvalidEvent(String),
    MissingEventStamp(u64),
    Projection(String),
    WriterBusy(PathBuf),
    OperationTimeout(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::IoContext {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} at {}: {source}", path.display()),
            Self::Audit(error) => error.fmt(formatter),
            Self::Fault(error) => error.fmt(formatter),
            Self::SecurityPolicy(error) => {
                write!(
                    formatter,
                    "security policy rejected ledger storage: {}: {}",
                    error.code, error.message
                )
            }
            Self::Capacity(reason) => write!(formatter, "storage capacity exceeded: {reason}"),
            Self::Integrity(reason) => write!(formatter, "audit integrity failure: {reason}"),
            Self::CaptureConflict => {
                formatter.write_str("display sources changed during verified capture")
            }
            Self::InvalidRecord(reason) => write!(formatter, "invalid audit record: {reason}"),
            Self::InvalidEvent(reason) => write!(formatter, "invalid client event: {reason}"),
            Self::MissingEventStamp(sequence) => write!(
                formatter,
                "event at sequence {sequence} lacks a historical stamp; refresh snapshots and rebase"
            ),
            Self::Projection(reason) => write!(formatter, "invalid state projection: {reason}"),
            Self::WriterBusy(path) => write!(
                formatter,
                "run ledger is already owned by another writer: {}",
                path.display()
            ),
            Self::OperationTimeout(reason) => {
                write!(formatter, "ledger operation timed out: {reason}")
            }
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

pub use crate::event::*;

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
    // A flushed group may be replaced before its scheduler observes the empty slot.
    epoch: u64,
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
    ledger_bytes: u64,
    generation_records: usize,
    _anchor_file: File,
    _workspace_lease: Option<crate::ledger_generation::WorkspaceLease>,
    durable_len: usize,
    projection: RunStateProjection,
    tail_projection: RunStateProjection,
    pending_since_millis: Option<u64>,
    poisoned: bool,
    clock: Arc<C>,
    replay_started_millis: u64,
    faults: Arc<F>,
    scheduled: Arc<ScheduledCommit<C, F>>,
    scheduler: Option<thread::JoinHandle<()>>,
    event_context: Option<EventAppendContext>,
    tail_interaction_revision: u64,
}

impl Ledger<SystemLedgerClock, NoFaults> {
    pub fn open(root: impl Into<PathBuf>, run_id: Uuid) -> Result<Self, LedgerError> {
        Self::open_with(root, run_id, SystemLedgerClock::default(), NoFaults)
    }
}

impl<C: LedgerClock, F: FaultInjector> Drop for Ledger<C, F> {
    fn drop(&mut self) {
        {
            let mut state = self
                .scheduled
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.cancelled = true;
            if let Some(pending) = state.pending.take() {
                let result = commit_projection(
                    &self.file,
                    &self.root,
                    &self.state_path,
                    &*self.faults,
                    &pending.projection,
                );
                match result {
                    Ok(()) => state.published = Some(pending.projection),
                    Err(error) => {
                        let message = error.to_string();
                        eprintln!(
                            "dolgorae: final ledger commit failed for run {} at {}: {message}",
                            self.run_id,
                            self.state_path.display()
                        );
                        state.error = Some(message);
                    }
                }
            }
        }
        if let Some(scheduler) = self.scheduler.take() {
            // Its duplicated descriptor must release the writer lock before reopen.
            let _ = scheduler.join();
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
        let workspace_lease = crate::ledger_generation::WorkspaceLease::for_run(&root)?;
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
        let anchor_file = OpenOptions::new()
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&audit_path)
            .map_err(|error| io_at("open audit ledger", &audit_path, error))?;
        DarwinSystem
            .lock_exclusive_nonblocking(&anchor_file)
            .map_err(|error| {
                if error
                    .raw_os_error()
                    .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
                {
                    LedgerError::WriterBusy(audit_path.clone())
                } else {
                    io_at("lock audit ledger", &audit_path, error)
                }
            })?;
        let replay_started = clock.monotonic_millis();
        let mut pin = crate::ledger_generation::pin(&root, run_id)?;
        let mut records = Vec::new();
        for mut historical in pin.historical.drain(..) {
            let (next, tail) = scan_ledger(
                &mut historical,
                &audit_path,
                run_id,
                &clock,
                replay_started,
                records,
            )?;
            if tail.is_some() {
                return Err(LedgerError::Integrity(
                    "immutable history segment has a torn tail".to_owned(),
                ));
            }
            records = next;
        }
        let generation_start = records.len();
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&pin.append_path)?;
        let (mut records, tail) = scan_ledger(
            &mut file,
            &pin.append_path,
            run_id,
            &clock,
            replay_started,
            records,
        )?;
        crate::ledger_generation::validate_checkpoint(&root, &pin, &records)?;
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
        let initial_projection = replay_with_budget(run_id, &records, &*clock, replay_started)?;
        let ledger_bytes = tail.as_ref().map_or(
            file.metadata()
                .map_err(|error| io_at("inspect audit ledger", &audit_path, error))?
                .len(),
            |tail| tail.prefix_length,
        );
        let ledger_bytes = ledger_bytes.saturating_add(pin.base_bytes);
        if ledger_bytes > MAX_LEDGER_BYTES {
            return Err(LedgerError::Capacity(
                "selected generation exceeds byte bound".to_owned(),
            ));
        }
        let generation_records = records.len() - generation_start;
        let tail_interaction_revision = interaction_state_revision(&records);
        let mut ledger = Self {
            run_id,
            root,
            state_path,
            recovery_path,
            file,
            projection: initial_projection.clone(),
            tail_projection: initial_projection,
            durable_len: records.len(),
            records: std::mem::take(&mut records),
            ledger_bytes,
            generation_records,
            _anchor_file: anchor_file,
            _workspace_lease: workspace_lease,
            pending_since_millis: None,
            poisoned: false,
            clock,
            replay_started_millis: replay_started,
            faults,
            scheduled,
            scheduler: None,
            event_context: None,
            tail_interaction_revision,
        };
        crate::ledger_generation::retain_activation(&ledger.root, run_id)?;
        if let Some(tail) = tail {
            ledger.preserve_and_truncate_tail(tail)?;
        }
        ledger.finish_pending_repairs()?;
        ledger.reconcile_projection()?;
        Ok(ledger)
    }

    /// The caller holds Writer observation serialization until the append is fsynced.
    pub fn with_event_context<T, E>(
        &mut self,
        context: EventAppendContext,
        append: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let previous = self.event_context.replace(context);
        let result = append(self);
        self.event_context = previous;
        result
    }

    pub(crate) fn replace_event_context(
        &mut self,
        context: Option<EventAppendContext>,
    ) -> Option<EventAppendContext> {
        std::mem::replace(&mut self.event_context, context)
    }

    /// Bind metadata before another durable authority hashes or validates this record.
    pub(crate) fn prepare_event(&self, record: AuditRecord) -> Result<AuditRecord, LedgerError> {
        Ok(if record.client_projection().is_none() {
            if let Some(context) = &self.event_context {
                let mut interaction_revision = self.tail_interaction_revision;
                interaction_revision = next_interaction_revision(interaction_revision, &record);
                match crate::event::project_audit_event(
                    &record,
                    &self.tail_projection,
                    context,
                    interaction_revision,
                    &self.records,
                )
                .map_err(|error| LedgerError::InvalidEvent(error.to_string()))?
                {
                    Some(event) => record.with_client_projection(event)?,
                    None => record,
                }
            } else {
                record
            }
        } else {
            record
        })
    }

    pub fn head(&self) -> Result<LedgerHead, LedgerError> {
        Ok(self.projection()?.ledger_head)
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

    pub fn projection(&self) -> Result<RunStateProjection, LedgerError> {
        self.check_scheduler_health()?;
        self.effective_projection()
    }

    pub fn try_projection(&self) -> Result<RunStateProjection, LedgerError> {
        self.projection()
    }

    pub fn try_head(&self) -> Result<LedgerHead, LedgerError> {
        self.head()
    }

    pub fn durable_records(&self) -> Result<&[AuditRecord], LedgerError> {
        self.check_scheduler_health()?;
        Ok(&self.records[..self.effective_durable_len()?])
    }

    pub fn try_durable_records(&self) -> Result<&[AuditRecord], LedgerError> {
        self.durable_records()
    }

    pub fn payloads_of_kind(&self, kind: AuditKind) -> Result<Vec<serde_json::Value>, LedgerError> {
        self.durable_records()?
            .iter()
            .filter(|record| record.kind() == kind)
            .map(|record| {
                let bytes = canonicalize(record.payload())
                    .map_err(|error| LedgerError::Integrity(error.to_string()))?;
                serde_json::from_slice(&bytes)
                    .map_err(|error| LedgerError::Integrity(error.to_string()))
            })
            .collect()
    }

    pub fn approval_decisions(&self) -> Result<Vec<serde_json::Value>, LedgerError> {
        self.durable_records()?
            .iter()
            .filter(|record| record.kind() == AuditKind::ApprovalDecided)
            .map(|record| {
                let bytes = canonicalize(record.payload())
                    .map_err(|error| LedgerError::Integrity(error.to_string()))?;
                let mut value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|error| LedgerError::Integrity(error.to_string()))?;
                // Early v2 nonsecret decisions omitted receipts. Reuse their
                // already committed public event identity without rewriting history.
                if value
                    .get("resolution_receipt_id")
                    .is_none_or(serde_json::Value::is_null)
                    && value
                        .get("response_sha256")
                        .is_some_and(serde_json::Value::is_string)
                    && matches!(
                        value
                            .pointer("/resolution/outcome")
                            .and_then(serde_json::Value::as_str),
                        Some("approval" | "answered")
                    )
                    && let Some(event) = record.client_projection()
                {
                    value["resolution_receipt_id"] = serde_json::json!(event.record.event_id);
                }
                Ok(value)
            })
            .collect()
    }

    pub(crate) fn check_open_replay_budget(&self) -> Result<(), LedgerError> {
        check_replay_budget(&*self.clock, self.replay_started_millis)
    }

    pub fn close(mut self) -> Result<(), LedgerError> {
        self.flush()
    }

    pub fn append(
        &mut self,
        record: AuditRecord,
        durability: AppendDurability,
    ) -> Result<(), LedgerError> {
        if matches!(
            record.kind(),
            AuditKind::StartFailed | AuditKind::CleanupResult | AuditKind::LifecycleTransition
        ) {
            return Err(LedgerError::InvalidRecord(
                "lifecycle evidence and transitions require the conformant authority boundary"
                    .to_owned(),
            ));
        }
        self.append_impl(record, durability)
    }

    pub(crate) fn append_conformance_record(
        &mut self,
        record: AuditRecord,
        durability: AppendDurability,
    ) -> Result<(), LedgerError> {
        if !matches!(
            record.kind(),
            AuditKind::StartFailed | AuditKind::CleanupResult | AuditKind::LifecycleTransition
        ) {
            return Err(LedgerError::InvalidRecord(
                "conformance append accepts only lifecycle evidence or transitions".to_owned(),
            ));
        }
        self.append_impl(record, durability)
    }

    fn append_impl(
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
        let record = self.prepare_event(record)?;
        let line = record.canonical_line()?;
        if line.len() > MAX_AUDIT_LINE_BYTES {
            return Err(LedgerError::InvalidRecord(
                "canonical audit line exceeds the bounded record limit".to_owned(),
            ));
        }
        let line_bytes = u64::try_from(line.len()).unwrap_or(u64::MAX);
        let protected_open = record.kind() == AuditKind::InteractionOpened
            && self.records.last().is_some_and(|prior| {
                prior.kind() == AuditKind::ApprovalRequested
                    && approval_request_matches(prior, &record)
            });
        let pending = self.tail_projection.pending_requests.len()
            + usize::from(
                record.kind() == AuditKind::ApprovalRequested
                    || (record.kind() == AuditKind::InteractionOpened && !protected_open),
            );
        let admission = matches!(
            record.kind(),
            AuditKind::TurnIntent | AuditKind::IdempotencyReserved | AuditKind::MutationAdmitted
        );
        check_record_capacity_with_reserve(
            self.ledger_bytes,
            self.generation_records,
            line_bytes,
            if record.kind() == AuditKind::InteractionOpened {
                protected_open
            } else if record.kind() == AuditKind::InteractionResolved {
                payload_string(record.payload(), "request_id")
                    .is_some_and(|id| self.tail_projection.pending_requests.contains(&id))
            } else {
                finalization_record(&record)
            },
            pending,
            admission,
        )?;
        let candidate_projection = project_next(self.run_id, &self.tail_projection, &record)
            .map_err(|error| {
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
            self.file.write_all(&line)?;
            self.faults.check(FaultBarrier::AfterLedgerAppend)?;
            Ok::<(), LedgerError>(())
        })();
        if let Err(error) = write_result {
            self.poisoned = true;
            scheduled_state.error = Some(error.to_string());
            return Err(error);
        }
        self.tail_interaction_revision =
            next_interaction_revision(self.tail_interaction_revision, &record);
        self.records.push(record);
        self.generation_records = self.generation_records.saturating_add(1);
        self.ledger_bytes = self.ledger_bytes.saturating_add(line_bytes);
        self.tail_projection = candidate_projection.clone();
        let started = *self
            .pending_since_millis
            .get_or_insert_with(|| self.clock.monotonic_millis());
        let mut scheduler_epoch = None;
        if durability == AppendDurability::Streaming {
            let deadline = scheduled_state
                .pending
                .as_ref()
                .map_or(started.saturating_add(GROUP_COMMIT_MILLIS), |pending| {
                    pending.deadline_millis
                });
            if scheduled_state.pending.is_none() {
                scheduled_state.epoch = scheduled_state.epoch.wrapping_add(1);
                scheduler_epoch = Some(scheduled_state.epoch);
            }
            scheduled_state.pending = Some(PendingCommit {
                deadline_millis: deadline,
                projection: candidate_projection,
            });
        }
        drop(scheduled_state);
        if let Some(epoch) = scheduler_epoch {
            if let Some(scheduler) = self.scheduler.take() {
                let _ = scheduler.join();
            }
            self.scheduler = Some(spawn_group_commit(Arc::clone(&scheduled), epoch));
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

    pub fn append_required_payload<T: serde::Serialize>(
        &mut self,
        kind: AuditKind,
        payload: &T,
        run_generation: u64,
    ) -> Result<(), LedgerError> {
        let serialized = serde_json::to_string(payload)
            .map_err(|error| LedgerError::InvalidRecord(error.to_string()))?;
        let record = AuditRecord::new(
            self.next_sequence(),
            self.clock.timestamp(),
            self.run_id,
            run_generation,
            kind,
            parse(&serialized).map_err(|error| LedgerError::InvalidRecord(error.to_string()))?,
            self.previous_hash(),
        )?;
        self.append(record, AppendDurability::Required)
    }

    pub(crate) fn append_conformance_payload<T: serde::Serialize>(
        &mut self,
        kind: AuditKind,
        payload: &T,
        run_generation: u64,
    ) -> Result<(), LedgerError> {
        let serialized = serde_json::to_string(payload)
            .map_err(|error| LedgerError::InvalidRecord(error.to_string()))?;
        let record = AuditRecord::new(
            self.next_sequence(),
            self.clock.timestamp(),
            self.run_id,
            run_generation,
            kind,
            parse(&serialized).map_err(|error| LedgerError::InvalidRecord(error.to_string()))?,
            self.previous_hash(),
        )?;
        self.append_conformance_record(record, AppendDurability::Required)
    }

    /// Emit terminal response and complete usage facts before the terminal transition.
    /// Legacy callers without append authority retain their original ledger shape.
    pub fn append_final_response_event(
        &mut self,
        terminal: &serde_json::Value,
        run_generation: u64,
    ) -> Result<(), LedgerError> {
        let Some(context) = self.event_context.clone() else {
            return Ok(());
        };
        let invalid = || {
            LedgerError::InvalidEvent("terminal event lacks typed response provenance".to_owned())
        };
        let mut data = Vec::new();
        let response = &terminal["final_response"];
        let final_response = match response["kind"].as_str() {
            Some("inline") => {
                let text = response["text"].as_str().ok_or_else(invalid)?;
                (!text.is_empty()).then(|| FinalResponse::Inline {
                    text: text.to_owned(),
                })
            }
            Some("artifact") => Some(FinalResponse::Artifact {
                artifact: Box::new(ArtifactMetadata {
                    schema_version: 1,
                    artifact_id: response["artifact_id"]
                        .as_str()
                        .and_then(|id| Uuid::parse_str(id).ok())
                        .ok_or_else(invalid)?,
                    run_id: self.run_id,
                    kind: "final_response".to_owned(),
                    visibility: "observer".to_owned(),
                    interaction_request_id: None,
                    media_type: "text/markdown".to_owned(),
                    byte_length: response["byte_length"].as_u64().ok_or_else(invalid)?,
                    sha256: response["sha256"].as_str().ok_or_else(invalid)?.to_owned(),
                    created_at: response["created_at"]
                        .as_str()
                        .ok_or_else(invalid)?
                        .to_owned(),
                    retention: "run_lifetime".to_owned(),
                    integrity: "unverified".to_owned(),
                }),
            }),
            Some("unavailable") | None => None,
            _ => return Err(invalid()),
        };
        if let Some(response) = final_response {
            data.push(ClientEventData::ResponseFinal(ResponseEventPayload {
                response,
            }));
        }
        if let (Some(input_tokens), Some(output_tokens)) = (
            terminal["usage"]["inputTokens"].as_u64(),
            terminal["usage"]["outputTokens"].as_u64(),
        ) {
            data.push(ClientEventData::UsageReported(UsagePayload {
                input_tokens,
                output_tokens,
            }));
        }
        for data in data {
            self.append_client_event(
                ClientEventRecord {
                    schema_version: 1,
                    event_schema_version: 1,
                    cursor: self.next_sequence().to_string(),
                    event_id: Uuid::now_v7(),
                    timestamp: self.clock.timestamp(),
                    workspace_id: context.workspace_id.clone(),
                    run_id: self.run_id,
                    thread_id: Some(
                        terminal["thread_id"]
                            .as_str()
                            .ok_or_else(invalid)?
                            .to_owned(),
                    ),
                    turn_id: Some(terminal["turn_id"].as_str().ok_or_else(invalid)?.to_owned()),
                    server_key: context.server_key.clone(),
                    server_epoch: context.server_epoch,
                    data,
                },
                run_generation,
                AppendDurability::Required,
            )?;
        }
        Ok(())
    }

    pub fn append_client_event(
        &mut self,
        record: ClientEventRecord,
        run_generation: u64,
        durability: AppendDurability,
    ) -> Result<(), LedgerError> {
        record
            .validate(self.run_id, self.next_sequence())
            .map_err(|error| LedgerError::InvalidEvent(error.to_string()))?;
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
        if raw.len() > RAW_PAYLOAD_LIMIT {
            return Err(LedgerError::InvalidRecord(
                "app-server payload exceeds the 2 MiB pre-parse limit".to_owned(),
            ));
        }
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
        if self.effective_durable_len()? == self.records.len() {
            self.durable_len = self.records.len();
            self.pending_since_millis = None;
            return Ok(());
        }
        let scheduled = Arc::clone(&self.scheduled);
        let mut scheduled_state = scheduled.state.lock().map_err(|_| {
            LedgerError::Integrity("group-commit state lock was poisoned".to_owned())
        })?;
        let projection = self.tail_projection.clone();
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
        let durable_head = self.projection()?.ledger_head.sequence;
        if after > durable_head {
            return Err(LedgerError::InvalidEvent(format!(
                "event cursor {after} is beyond durable ledger head {durable_head}"
            )));
        }
        deliveries_after(
            self.durable_records()?,
            after,
            projection,
            replay_delivery,
            MAX_EVENT_DELIVERIES,
        )
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

    fn effective_durable_len(&self) -> Result<usize, LedgerError> {
        let state = self.scheduled.state.lock().map_err(|_| {
            LedgerError::Integrity("group-commit state lock was poisoned".to_owned())
        })?;
        Ok(state
            .published
            .as_ref()
            .map(|value| value.ledger_head.sequence)
            .and_then(|sequence| usize::try_from(sequence).ok())
            .unwrap_or(self.durable_len)
            .max(self.durable_len)
            .min(self.records.len()))
    }

    fn effective_projection(&self) -> Result<RunStateProjection, LedgerError> {
        let state = self.scheduled.state.lock().map_err(|_| {
            LedgerError::Integrity("group-commit state lock was poisoned".to_owned())
        })?;
        Ok(state
            .published
            .clone()
            .filter(|projection| {
                projection.ledger_head.sequence > self.projection.ledger_head.sequence
            })
            .unwrap_or_else(|| self.projection.clone()))
    }

    fn check_scheduler_health(&self) -> Result<(), LedgerError> {
        let state = self.scheduled.state.lock().map_err(|_| {
            LedgerError::Integrity("group-commit state lock was poisoned".to_owned())
        })?;
        if let Some(error) = &state.error {
            return Err(LedgerError::Integrity(error.clone()));
        }
        Ok(())
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
                LedgerError::Integrity(
                    "recovery evidence name is not valid UTF-8; inspect the recovery directory and remove only a verified foreign entry"
                        .to_owned(),
                )
            })?;
            if name.starts_with(".tail-evidence-") && name.ends_with(".tmp") {
                verify_secure_file(&entry.path(), current_uid()).map_err(machine_as_security)?;
                fs::remove_file(entry.path())?;
                removed_temporary = true;
                continue;
            }
            let Some((sequence, digest)) = parse_tail_evidence_name(&name) else {
                return Err(LedgerError::Integrity(format!(
                    "unrecognized recovery entry: {name}; inspect {} and remove it only after verifying that it is not Dolgorae recovery evidence",
                    self.recovery_path.display()
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
        self.check_open_replay_budget()?;
        let rebuilt = self.tail_projection.clone();
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
    epoch: u64,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        loop {
            let deadline = match scheduled.state.lock() {
                Ok(state) if state.cancelled || state.epoch != epoch => return,
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
            if state.cancelled || state.epoch != epoch {
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
    })
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

fn scan_ledger<C: LedgerClock>(
    file: &mut File,
    audit_path: &Path,
    run_id: Uuid,
    clock: &C,
    started_millis: u64,
    mut records: Vec<AuditRecord>,
) -> Result<(Vec<AuditRecord>, Option<TornTail>), LedgerError> {
    let ledger_bytes = file
        .metadata()
        .map_err(|error| io_at("inspect audit ledger", audit_path, error))?
        .len();
    if ledger_bytes > MAX_LEDGER_BYTES {
        return Err(LedgerError::Capacity(format!(
            "audit ledger exceeds the {MAX_LEDGER_BYTES}-byte replay bound"
        )));
    }
    let mut reader = BufReader::new(file.try_clone()?);
    reader.seek(SeekFrom::Start(0))?;
    let generation_start = records.len();
    let mut prefix_length = 0_u64;
    let mut tail = None;
    while let Some((mut line, terminated)) = read_bounded_segment(&mut reader)? {
        if clock.monotonic_millis().saturating_sub(started_millis) >= REPLAY_BUDGET_MILLIS {
            return Err(LedgerError::OperationTimeout(format!(
                "full replay exceeded {REPLAY_BUDGET_MILLIS} milliseconds"
            )));
        }
        if records.len() - generation_start >= MAX_LEDGER_RECORDS {
            return Err(LedgerError::Capacity(format!(
                "audit ledger exceeds the {MAX_LEDGER_RECORDS}-record replay bound"
            )));
        }
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
        check_replay_budget(clock, started_millis)?;
    }
    check_replay_budget(clock, started_millis)?;
    file.seek(SeekFrom::End(0))?;
    Ok((records, tail))
}

#[cfg(test)]
fn check_append_capacity(
    current_bytes: u64,
    current_records: usize,
    line_bytes: u64,
) -> Result<(), LedgerError> {
    check_record_capacity(current_bytes, current_records, line_bytes, true)
}

#[cfg(test)]
fn check_record_capacity(
    current_bytes: u64,
    current_records: usize,
    line_bytes: u64,
    finalization: bool,
) -> Result<(), LedgerError> {
    check_record_capacity_with_reserve(
        current_bytes,
        current_records,
        line_bytes,
        finalization,
        0,
        false,
    )
}
fn check_record_capacity_with_reserve(
    current_bytes: u64,
    current_records: usize,
    line_bytes: u64,
    finalization: bool,
    pending_interactions: usize,
    admission: bool,
) -> Result<(), LedgerError> {
    let extra_records = pending_interactions
        .saturating_mul(2)
        .saturating_add(if admission { 2 } else { 0 });
    let extra_bytes = (extra_records as u64).saturating_mul(MAX_AUDIT_LINE_BYTES as u64);
    let (byte_limit, record_limit) = if finalization {
        (MAX_LEDGER_BYTES, MAX_LEDGER_RECORDS)
    } else {
        (
            MAX_LEDGER_BYTES
                .saturating_sub(FINALIZATION_RESERVED_BYTES.saturating_add(extra_bytes)),
            MAX_LEDGER_RECORDS
                .saturating_sub(FINALIZATION_RESERVED_RECORDS.saturating_add(extra_records)),
        )
    };
    if current_records >= record_limit || current_bytes.saturating_add(line_bytes) > byte_limit {
        return Err(LedgerError::Capacity(format!(
            "selected generation admission limit reached ({current_bytes} bytes, {current_records} records); operator checkpoint required"
        )));
    }
    Ok(())
}

fn approval_request_matches(prior: &AuditRecord, opened: &AuditRecord) -> bool {
    let Some(request) = payload_string(opened.payload(), "request_id") else {
        return false;
    };
    let LosslessJson::Object(entries) = prior.payload() else {
        return false;
    };
    entries.iter().any(|(key, value)| {
        key == "upstream_request_id"
            && canonicalize(value).is_ok_and(|bytes| bytes == request.as_bytes())
    })
}

fn finalization_record(record: &AuditRecord) -> bool {
    match record.kind() {
        AuditKind::ThreadBound
        | AuditKind::TurnStarted
        | AuditKind::TurnTerminal
        | AuditKind::MutationCompleted
        | AuditKind::OutcomeUnknown
        | AuditKind::Reconciliation
        | AuditKind::CleanupIntent
        | AuditKind::CleanupResult
        | AuditKind::WriterReleased
        | AuditKind::RunGenerationStopped
        | AuditKind::StartFailed
        | AuditKind::LedgerTailRepaired
        | AuditKind::ProjectionRewound
        | AuditKind::ApprovalDecided
        | AuditKind::WriterHandoffCancelled
        | AuditKind::WriterHandoffCompleted => true,
        AuditKind::TurnIntent => {
            payload_string(record.payload(), "schema").as_deref()
                == Some("dolgorae.submit-refusal/v1")
                || payload_string(record.payload(), "provisional_thread_id").is_some()
        }
        AuditKind::LifecycleTransition => !matches!(
            payload_string(record.payload(), "current").as_deref(),
            Some("running" | "starting")
        ),
        AuditKind::MutationAdmitted => {
            matches!(
                payload_string(record.payload(), "operation").as_deref(),
                Some(
                    "close"
                        | "interrupt"
                        | "release_write"
                        | "recover"
                        | "reconcile"
                        | "run.close"
                        | "run.interrupt"
                        | "run.release_write"
                        | "run.recover"
                        | "run.reconcile"
                )
            )
        }
        AuditKind::ClientEvent => event_from_payload(record.payload()).is_ok_and(|event| {
            matches!(
                event.data,
                ClientEventData::ResponseFinal(_) | ClientEventData::UsageReported(_)
            )
        }),
        _ => false,
    }
}

pub(crate) fn project(
    run_id: Uuid,
    records: &[AuditRecord],
) -> Result<RunStateProjection, LedgerError> {
    replay_with_budget(run_id, records, &SystemLedgerClock::default(), 0)
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

fn replay_with_budget<C: LedgerClock>(
    run_id: Uuid,
    records: &[AuditRecord],
    clock: &C,
    started_millis: u64,
) -> Result<RunStateProjection, LedgerError> {
    let mut projection = RunStateProjection::empty(run_id);
    for record in records {
        projection = project_next(run_id, &projection, record)?;
        check_replay_budget(clock, started_millis)?;
    }
    check_replay_budget(clock, started_millis)?;
    Ok(projection)
}

fn check_replay_budget<C: LedgerClock>(clock: &C, started_millis: u64) -> Result<(), LedgerError> {
    if clock.monotonic_millis().saturating_sub(started_millis) >= REPLAY_BUDGET_MILLIS {
        return Err(LedgerError::OperationTimeout(format!(
            "full replay exceeded {REPLAY_BUDGET_MILLIS} milliseconds"
        )));
    }
    Ok(())
}

fn project_next(
    run_id: Uuid,
    current: &RunStateProjection,
    record: &AuditRecord,
) -> Result<RunStateProjection, LedgerError> {
    crate::projection::project_next(run_id, current, record, projection_event_cursor)
        .map_err(LedgerError::Integrity)
}

fn projection_event_cursor(record: &AuditRecord) -> Result<String, String> {
    let event = event_from_payload(record.payload()).map_err(|error| error.to_string())?;
    event
        .validate(record.run_id(), record.sequence())
        .map_err(|error| format!("stored client event is invalid: {error}"))?;
    Ok(event.cursor)
}
fn event_from_payload(payload: &LosslessJson) -> Result<ClientEventRecord, LedgerError> {
    crate::event::from_payload(payload).map_err(|error| LedgerError::Integrity(error.to_string()))
}

/// One bounded page of client-event deliveries strictly after `after`.
///
/// Shared by the writer-side ledger and the observer read below, so an
/// observer that never takes the ledger lock still applies the same projection
/// membership, count bound, and byte bound as the worker does.
fn deliveries_after(
    records: &[AuditRecord],
    after: u64,
    projection: EventProjection,
    replay_delivery: bool,
    max_deliveries: usize,
) -> Result<Vec<EventDelivery>, LedgerError> {
    let mut deliveries = Vec::new();
    let mut delivery_bytes = 0_usize;
    let first = records.partition_point(|record| record.sequence() <= after);
    for record in &records[first..] {
        let event = if let Some(projection) = record.client_projection() {
            projection.record.clone()
        } else if record.kind() == AuditKind::ClientEvent {
            event_from_payload(record.payload())?
        } else {
            continue;
        };
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
            && (deliveries.len() == max_deliveries
                || delivery_bytes.saturating_add(encoded_length) > MAX_EVENT_DELIVERY_BYTES)
        {
            break;
        }
        delivery_bytes = delivery_bytes.saturating_add(encoded_length);
        deliveries.push(delivery);
    }
    Ok(deliveries)
}

/// Read one page of durable events without owning the Run's ledger.
///
/// docs/specs/README.md: projection-only `status`, `events`, and `verify` "read the
/// fsynced ledger/runtime projection directly and do not start, attach,
/// recover, or contend on the per-run startup lock". So this opens
/// `audit.jsonl` read-only, takes no advisory lock, repairs nothing, and stops
/// at `head` — the sequence the fsynced projection commits — which is exactly
/// the durable prefix a live worker would have served. A torn or unterminated
/// tail past that head is another process's in-flight append, not corruption,
/// so it is ignored rather than reported.
#[derive(Clone, Debug)]
pub struct ObservedLedger {
    records: Arc<Vec<AuditRecord>>,
    head: u64,
    derived: Arc<ObservedLedgerDerived>,
}

/// Derived display views belong to exactly one verified captured head.
#[derive(Debug, Default)]
struct ObservedLedgerDerived {
    interaction_revision: OnceLock<u64>,
    terminal: OnceLock<Result<Option<serde_json::Value>, String>>,
    policy: OnceLock<Result<Arc<ObservedPolicyInputs>, String>>,
}

#[derive(Debug)]
pub(crate) struct ObservedPolicyInputs {
    pub bindings: Vec<serde_json::Value>,
    pub policies: Vec<serde_json::Value>,
}

const DISPLAY_CACHE_ENTRIES: usize = 16;
const DISPLAY_CACHE_BYTES: u64 = 32 * 1024 * 1024;
const DISPLAY_CACHE_LIFETIME: Duration = Duration::from_secs(120);
static OBSERVATION_FULL_REPLAYS: AtomicU64 = AtomicU64::new(0);
static OBSERVATION_TAIL_RECORDS: AtomicU64 = AtomicU64::new(0);
static OBSERVATION_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static OBSERVATION_INVALIDATIONS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct LedgerObservationCounters {
    pub full_replays: u64,
    pub tail_records: u64,
    pub cache_hits: u64,
    pub invalidations: u64,
}

#[must_use]
pub fn observation_counters() -> LedgerObservationCounters {
    LedgerObservationCounters {
        full_replays: OBSERVATION_FULL_REPLAYS.load(Ordering::Relaxed),
        tail_records: OBSERVATION_TAIL_RECORDS.load(Ordering::Relaxed),
        cache_hits: OBSERVATION_CACHE_HITS.load(Ordering::Relaxed),
        invalidations: OBSERVATION_INVALIDATIONS.load(Ordering::Relaxed),
    }
}

struct DisplayPrefix {
    sources: Vec<crate::ledger_generation::ObservationFileIdentity>,
    referenced: Vec<PathBuf>,
    records: Arc<Vec<AuditRecord>>,
    derived: Arc<ObservedLedgerDerived>,
    append_path: PathBuf,
    append_offset: u64,
    append_hash: String,
    retained_bytes: u64,
    base_bytes: u64,
    watermark: u64,
    last_used: Instant,
}

type DisplayCacheSlot = Arc<Mutex<Option<DisplayPrefix>>>;
type DisplayCache = Vec<((PathBuf, Uuid), DisplayCacheSlot)>;
static DISPLAY_CACHE: OnceLock<Mutex<DisplayCache>> = OnceLock::new();

fn display_slot(root: &Path, run_id: Uuid) -> Result<DisplayCacheSlot, LedgerError> {
    // Canonicalize only after checking the requested directory: a symlink must
    // not become a valid cache alias by canonicalization.
    verify_secure_directory(root, current_uid()).map_err(machine_as_security)?;
    let key = (fs::canonicalize(root)?, run_id);
    let mut cache = DISPLAY_CACHE
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| LedgerError::Integrity("display cache poisoned".to_owned()))?;
    cache.retain(|(_, slot)| {
        // Never block the global registry on another Run's file work.
        slot.try_lock().map_or(true, |prefix| {
            prefix
                .as_ref()
                .is_none_or(|prefix| prefix.last_used.elapsed() < DISPLAY_CACHE_LIFETIME)
        })
    });
    if let Some(index) = cache.iter().position(|(candidate, _)| candidate == &key) {
        let entry = cache.remove(index);
        let slot = Arc::clone(&entry.1);
        cache.push(entry);
        return Ok(slot);
    }
    if cache.len() >= DISPLAY_CACHE_ENTRIES {
        cache.remove(0);
    }
    let slot = Arc::new(Mutex::new(None));
    cache.push((key, Arc::clone(&slot)));
    Ok(slot)
}

fn display_append_prefix(path: &Path, length: u64) -> Result<String, LedgerError> {
    verify_secure_file(path, current_uid()).map_err(machine_as_security)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut hash = Sha256::new();
    let mut remaining = length;
    let mut buffer = [0; 64 * 1024];
    while remaining != 0 {
        let amount = remaining.min(buffer.len() as u64) as usize;
        let n = file.read(&mut buffer[..amount])?;
        if n == 0 {
            return Err(LedgerError::Integrity(
                "verified display prefix was truncated".to_owned(),
            ));
        }
        hash.update(&buffer[..n]);
        remaining -= n as u64;
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn display_extend(
    prefix: &DisplayPrefix,
    run_id: Uuid,
    head: u64,
) -> Result<(Arc<Vec<AuditRecord>>, u64, String), LedgerError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&prefix.append_path)?;
    if file.metadata()?.len().saturating_add(prefix.base_bytes) > MAX_LEDGER_BYTES
        || head.saturating_sub(prefix.watermark) > MAX_LEDGER_RECORDS as u64
    {
        return Err(LedgerError::Capacity(
            "selected generation exceeds replay bounds".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(prefix.append_offset))?;
    let mut reader = BufReader::new(file);
    let mut records = prefix.records.as_ref().clone();
    let mut offset = prefix.append_offset;
    while records.len() < usize::try_from(head).unwrap_or(usize::MAX) {
        let Some((mut line, terminated)) = read_bounded_segment(&mut reader)? else {
            return Err(LedgerError::Integrity(
                "published suffix is below the committed head".to_owned(),
            ));
        };
        if !terminated {
            return Err(LedgerError::Integrity(
                "published suffix has a torn record".to_owned(),
            ));
        }
        offset += line.len() as u64;
        line.pop();
        let record = AuditRecord::from_canonical_line(&line)
            .map_err(|error| LedgerError::Integrity(error.to_string()))?;
        if record.run_id() != run_id
            || record.sequence() != records.len() as u64 + 1
            || record.previous_hash()
                != records
                    .last()
                    .map_or(GENESIS_PREVIOUS_HASH, AuditRecord::hash)
        {
            return Err(LedgerError::Integrity(
                "display suffix does not continue the verified chain".to_owned(),
            ));
        }
        records.push(record);
    }
    let hash = display_append_prefix(&prefix.append_path, offset)?;
    Ok((Arc::new(records), offset, hash))
}

#[cfg(test)]
type DisplayCaptureFenceHook = Option<(Uuid, Box<dyn FnMut()>)>;
#[cfg(test)]
thread_local! {
    static DISPLAY_CAPTURE_FENCE_HOOK: std::cell::RefCell<DisplayCaptureFenceHook> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn display_capture_test_fence(run_id: Uuid) {
    DISPLAY_CAPTURE_FENCE_HOOK.with(|selected| {
        if let Some((selected_run, hook)) = selected.borrow_mut().as_mut()
            && *selected_run == run_id
        {
            hook();
        }
    });
}

impl ObservedLedger {
    /// Display-only verified prefix reuse. Never use this to establish absence,
    /// authorize a mutation, release a writer or replace a worker generation.
    pub fn open_display(root: &Path, run_id: Uuid, head: u64) -> Result<Self, LedgerError> {
        let slot = display_slot(root, run_id)?;
        let mut slot = slot
            .lock()
            .map_err(|_| LedgerError::Integrity("display prefix poisoned".to_owned()))?;
        let mut captured_head_hash: Option<String> = None;
        for _ in 0..3 {
            let sources = crate::ledger_generation::observation_identity(
                root,
                slot.as_ref()
                    .map_or(&[], |prefix| prefix.referenced.as_slice()),
            )?;
            if let Some(prefix) = slot.as_mut() {
                let same = sources == prefix.sources;
                if prefix.last_used.elapsed() < DISPLAY_CACHE_LIFETIME {
                    if same && head <= prefix.records.len() as u64 {
                        prefix.last_used = Instant::now();
                        OBSERVATION_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
                        return Ok(Self {
                            records: Arc::clone(&prefix.records),
                            head,
                            derived: if head == prefix.records.len() as u64 {
                                Arc::clone(&prefix.derived)
                            } else {
                                Arc::new(ObservedLedgerDerived::default())
                            },
                        });
                    }
                    // Only the selected append may change in an extension. File
                    // replacement, permissions and every immutable source remain fenced.
                    let append_only = sources.len() == prefix.sources.len()
                        && sources.iter().zip(&prefix.sources).all(|(new, old)| {
                            if new.path != prefix.append_path {
                                return new == old;
                            }
                            new.path == old.path
                                && new.device == old.device
                                && new.inode == old.inode
                                && new.mode == old.mode
                                && new.uid == old.uid
                                && new.links == old.links
                                && new.length >= prefix.append_offset
                        });
                    if append_only
                        && head >= prefix.records.len() as u64
                        && display_append_prefix(&prefix.append_path, prefix.append_offset)?
                            == prefix.append_hash
                    {
                        let old_len = prefix.records.len();
                        let (records, offset, hash) = display_extend(prefix, run_id, head)?;
                        captured_head_hash = records.last().map(|record| record.hash().to_owned());
                        #[cfg(test)]
                        display_capture_test_fence(run_id);
                        if crate::ledger_generation::observation_identity(root, &prefix.referenced)?
                            != sources
                        {
                            continue;
                        }
                        OBSERVATION_TAIL_RECORDS
                            .fetch_add((records.len() - old_len) as u64, Ordering::Relaxed);
                        let additional_bytes = offset.saturating_sub(prefix.append_offset);
                        if prefix.retained_bytes.saturating_add(additional_bytes)
                            > DISPLAY_CACHE_BYTES
                        {
                            *slot = None;
                            return Ok(Self {
                                records,
                                head,
                                derived: Arc::new(ObservedLedgerDerived::default()),
                            });
                        }
                        prefix.retained_bytes += additional_bytes;
                        prefix.sources = sources;
                        prefix.records = records;
                        prefix.derived = Arc::new(ObservedLedgerDerived::default());
                        prefix.append_offset = offset;
                        prefix.append_hash = hash;
                        prefix.last_used = Instant::now();
                        return Ok(Self {
                            records: Arc::clone(&prefix.records),
                            head,
                            derived: Arc::clone(&prefix.derived),
                        });
                    }
                }
                OBSERVATION_INVALIDATIONS.fetch_add(1, Ordering::Relaxed);
            }
            *slot = None;
            let pin = crate::ledger_generation::pin(root, run_id)?;
            let sources =
                crate::ledger_generation::observation_identity(root, &pin.observation_paths)?;
            let records = Arc::new(observe_records(root, run_id, head, false)?);
            captured_head_hash = records.last().map(|record| record.hash().to_owned());
            let watermark = pin
                .manifest
                .as_ref()
                .map_or(0, |manifest| manifest.durable_watermark);
            let mut offset = 0_u64;
            let mut retained_bytes = 0_u64;
            for record in records.iter() {
                let bytes = record.canonical_line()?.len() as u64;
                retained_bytes += bytes;
                if record.sequence() > watermark {
                    offset += bytes;
                }
            }
            let hash = display_append_prefix(&pin.append_path, offset)?;
            #[cfg(test)]
            display_capture_test_fence(run_id);
            if crate::ledger_generation::observation_identity(root, &pin.observation_paths)?
                != sources
            {
                continue;
            }
            let derived = Arc::new(ObservedLedgerDerived::default());
            if head >= watermark && retained_bytes <= DISPLAY_CACHE_BYTES {
                *slot = Some(DisplayPrefix {
                    sources,
                    referenced: pin.observation_paths,
                    records: Arc::clone(&records),
                    derived: Arc::clone(&derived),
                    append_path: pin.append_path,
                    append_offset: offset,
                    append_hash: hash,
                    retained_bytes,
                    base_bytes: pin.base_bytes,
                    watermark,
                    last_used: Instant::now(),
                });
            }
            return Ok(Self {
                records,
                head,
                derived,
            });
        }
        // Motion at the final fence must not hide damage to the committed
        // prefix. Revalidate the selected history before reporting a transient;
        // this never returns a prefix or establishes authoritative absence.
        let current = observe_records(root, run_id, head, false)?;
        if current.last().map(AuditRecord::hash) != captured_head_hash.as_deref() {
            return Err(LedgerError::Integrity(
                "captured display prefix changed during source motion".to_owned(),
            ));
        }
        Err(LedgerError::CaptureConflict)
    }

    pub(crate) fn open_run_display(
        state_root: &Path,
        run_id: Uuid,
        head: u64,
    ) -> Result<Self, LedgerError> {
        Self::open_display(
            &state_root.join("runs").join(run_id.to_string()),
            run_id,
            head,
        )
    }

    /// Shared adapter admission for the same committed Run prefix.
    pub(crate) fn open_run(
        state_root: &Path,
        run_id: Uuid,
        head: u64,
    ) -> Result<Self, crate::machine::MachineError> {
        Self::open(
            &state_root.join("runs").join(run_id.to_string()),
            run_id,
            head,
        )
        .map_err(|error| match error {
            LedgerError::Capacity(reason) => crate::machine::MachineError::new(
                "STORAGE_CAPACITY_EXCEEDED",
                reason,
                false,
                serde_json::json!({"run_id": run_id, "required_action": "operator_checkpoint"}),
            ),
            other => audit_integrity_error(run_id, head, &other.to_string()),
        })
    }

    pub fn open(root: &Path, run_id: Uuid, head: u64) -> Result<Self, LedgerError> {
        Ok(Self {
            records: Arc::new(observe_records(root, run_id, head, false)?),
            head,
            derived: Arc::new(ObservedLedgerDerived::default()),
        })
    }

    /// Require the published prefix to cover every retained byte. Operation
    /// absence cannot be inferred from a prefix with an unclassified tail.
    pub fn open_complete(root: &Path, run_id: Uuid, head: u64) -> Result<Self, LedgerError> {
        Ok(Self {
            records: Arc::new(observe_records(root, run_id, head, true)?),
            head,
            derived: Arc::new(ObservedLedgerDerived::default()),
        })
    }

    #[must_use]
    pub(crate) fn records(&self) -> &[AuditRecord] {
        &self.records[..usize::try_from(self.head).expect("verified head fits usize")]
    }

    #[must_use]
    pub const fn head(&self) -> u64 {
        self.head
    }

    /// The durable sequence that last invalidated this Run's Interaction view.
    #[must_use]
    pub fn interaction_state_revision(&self) -> u64 {
        *self
            .derived
            .interaction_revision
            .get_or_init(|| interaction_state_revision(self.records()))
    }

    /// Export this already selected, verified prefix without changing sequence or hash bytes.
    /// Transport callers retain their existing Controller authorization obligation.
    pub fn write_canonical_history(
        &self,
        output: &mut impl std::io::Write,
    ) -> Result<(), LedgerError> {
        for record in self.records() {
            output.write_all(&record.canonical_line()?)?;
        }
        Ok(())
    }

    /// Read historical typed events without borrowing current snapshot revisions.
    pub fn stamped_events_after(
        &self,
        after: u64,
        projection: EventProjection,
    ) -> Result<Vec<StampedClientEvent>, LedgerError> {
        let mut events = Vec::new();
        let mut bytes = 0_usize;
        for audit in self
            .records()
            .iter()
            .filter(|record| record.sequence() > after)
        {
            let Some(event) = audit.client_projection() else {
                if audit.kind() == AuditKind::ClientEvent {
                    let legacy = event_from_payload(audit.payload())?;
                    if projection != EventProjection::Minimal || legacy.data.minimal() {
                        return Err(LedgerError::MissingEventStamp(audit.sequence()));
                    }
                }
                continue;
            };
            if projection == EventProjection::Minimal && !event.record.data.minimal() {
                continue;
            }
            let encoded = serde_json::to_vec(event)
                .map_err(|error| LedgerError::InvalidEvent(error.to_string()))?
                .len();
            if !events.is_empty()
                && (events.len() == MAX_EVENT_DELIVERIES
                    || bytes.saturating_add(encoded) > MAX_EVENT_DELIVERY_BYTES)
            {
                break;
            }
            bytes = bytes.saturating_add(encoded);
            events.push(event.clone());
        }
        Ok(events)
    }

    /// The payload of the last durable `turn_terminal` record, if this Run
    /// has one.
    ///
    /// docs/specs/README.md has a Master read `run status.data.last_terminal` for the
    /// response, usage, and cursor behind an exit-7 envelope. The ledger is
    /// where that fact is durable, so an observer with no live worker
    /// reconstructs the terminal Turn from the record rather than from a
    /// worker's process memory.
    pub fn last_terminal(&self) -> Result<Option<serde_json::Value>, LedgerError> {
        self.derived
            .terminal
            .get_or_init(|| {
                self.decode_last_terminal()
                    .map_err(|error| error.to_string())
            })
            .clone()
            .map_err(LedgerError::Integrity)
    }

    pub(crate) fn policy_inputs(&self) -> Result<Arc<ObservedPolicyInputs>, LedgerError> {
        self.derived
            .policy
            .get_or_init(|| {
                let bindings = self
                    .payloads_of_kind(AuditKind::ThreadBound)
                    .map_err(|error| error.to_string())?;
                let mut policies = bindings.clone();
                for kind in [AuditKind::WriterAcquired, AuditKind::WriterReleased] {
                    policies.extend(
                        self.payloads_of_kind(kind)
                            .map_err(|error| error.to_string())?,
                    );
                }
                Ok(Arc::new(ObservedPolicyInputs { bindings, policies }))
            })
            .clone()
            .map_err(LedgerError::Integrity)
    }

    fn decode_last_terminal(&self) -> Result<Option<serde_json::Value>, LedgerError> {
        let Some(record) = self
            .records()
            .iter()
            .rev()
            .find(|record| record.kind() == AuditKind::TurnTerminal)
        else {
            return Ok(None);
        };
        let bytes = canonicalize(record.payload())
            .map_err(|error| LedgerError::Integrity(error.to_string()))?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| LedgerError::Integrity(error.to_string()))
    }

    pub fn payloads_of_kind(&self, kind: AuditKind) -> Result<Vec<serde_json::Value>, LedgerError> {
        self.records()
            .iter()
            .filter(|record| record.kind() == kind)
            .map(|record| {
                let bytes = canonicalize(record.payload())
                    .map_err(|error| LedgerError::Integrity(error.to_string()))?;
                serde_json::from_slice(&bytes)
                    .map_err(|error| LedgerError::Integrity(error.to_string()))
            })
            .collect()
    }

    /// One page of durable deliveries strictly after `after`, bounded exactly
    /// as the worker's own page is so both paths page identically.
    pub fn events_after(
        &self,
        after: u64,
        projection: EventProjection,
    ) -> Result<Vec<EventDelivery>, LedgerError> {
        deliveries_after(
            self.records(),
            after,
            projection,
            true,
            MAX_EVENT_DELIVERIES,
        )
    }
}

pub(crate) fn parse_event_cursor(value: &str) -> Option<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|cursor| cursor.to_string() == value)
}

pub(crate) fn event_cursor_invalid(
    run_id: Uuid,
    requested: &str,
    head: &str,
) -> crate::machine::MachineError {
    crate::machine::MachineError::new(
        "EVENT_CURSOR_INVALID",
        "event cursor is noncanonical or beyond the run ledger head",
        false,
        serde_json::json!({"run_id":run_id,"requested_cursor":requested,"head_cursor":head}),
    )
}

pub(crate) fn display_observation_error(
    run_id: Uuid,
    head: u64,
    lifecycle: RunLifecycle,
    error: LedgerError,
) -> crate::machine::MachineError {
    match error {
        LedgerError::CaptureConflict => crate::machine::MachineError::new(
            "RUN_STATE_CONFLICT",
            "run state changed",
            false,
            serde_json::json!({"run_id":run_id,"state":lifecycle.as_str(),"operation":"capture_snapshot"}),
        ),
        LedgerError::Capacity(reason) => crate::machine::MachineError::new(
            "STORAGE_CAPACITY_EXCEEDED",
            reason,
            false,
            serde_json::json!({"run_id":run_id,"required_action":"operator_checkpoint"}),
        ),
        other => audit_integrity_error(run_id, head, &other.to_string()),
    }
}

pub(crate) fn audit_integrity_error(
    run_id: Uuid,
    sequence: u64,
    reason: &str,
) -> crate::machine::MachineError {
    crate::machine::MachineError::new(
        "AUDIT_INTEGRITY_FAILURE",
        "run ledger could not be replayed",
        false,
        serde_json::json!({"run_id":run_id,"sequence":sequence,"reason":reason}),
    )
}

fn interaction_state_revision(records: &[AuditRecord]) -> u64 {
    let mut revision = 0;
    for record in records {
        revision = next_interaction_revision(revision, record);
    }
    revision
}

fn next_interaction_revision(revision: u64, record: &AuditRecord) -> u64 {
    let changes_interactions = match record.kind() {
        AuditKind::ApprovalRequested
        | AuditKind::ApprovalDecided
        | AuditKind::InteractionOpened
        | AuditKind::InteractionResolved => true,
        AuditKind::RunGenerationStarted
        | AuditKind::RunGenerationStopped
        | AuditKind::ControllerReset
        | AuditKind::LifecycleTransition
        | AuditKind::Reconciliation
        | AuditKind::OutcomeUnknown
        | AuditKind::StartFailed => revision != 0,
        AuditKind::AppServerNotification => {
            revision != 0
                && matches!(record.payload(),
                LosslessJson::Object(fields) if fields.iter().any(|(key, value)|
                    key == "method" && matches!(value, LosslessJson::String(method)
                        if method == "item/fileChange/patchUpdated")))
        }
        _ => false,
    };
    if changes_interactions {
        record.sequence()
    } else {
        revision
    }
}

/// The durable record prefix through `head`, verified as it is read.
fn observe_records(
    root: &Path,
    run_id: Uuid,
    head: u64,
    require_complete: bool,
) -> Result<Vec<AuditRecord>, LedgerError> {
    OBSERVATION_FULL_REPLAYS.fetch_add(1, Ordering::Relaxed);
    if run_id.get_version_num() != 7 {
        return Err(LedgerError::InvalidRecord(
            "run identity must be UUIDv7".to_owned(),
        ));
    }
    let uid = current_uid();
    verify_secure_directory(root, uid).map_err(machine_as_security)?;
    let pin = crate::ledger_generation::pin(root, run_id)?;
    let mut pin = pin;
    let mut files = std::mem::take(&mut pin.historical);
    verify_secure_file(&pin.append_path, uid).map_err(machine_as_security)?;
    files.push(
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&pin.append_path)?,
    );
    for file in &files {
        if file.metadata()?.len() > MAX_LEDGER_BYTES {
            return Err(LedgerError::Capacity(
                "selected segment exceeds replay bound".to_owned(),
            ));
        }
    }
    if files
        .last()
        .expect("selected append file")
        .metadata()?
        .len()
        .saturating_add(pin.base_bytes)
        > MAX_LEDGER_BYTES
        || head.saturating_sub(pin.manifest.as_ref().map_or(0, |m| m.durable_watermark))
            > MAX_LEDGER_RECORDS as u64
    {
        return Err(LedgerError::Capacity(
            "selected generation exceeds replay bounds".to_owned(),
        ));
    }
    let mut reader = GenerationReader {
        files: files.into_iter().map(BufReader::new).collect(),
        current: 0,
    };
    let mut records: Vec<AuditRecord> = Vec::new();
    while records.len() < usize::try_from(head).unwrap_or(usize::MAX) {
        let Some((mut line, terminated)) = read_bounded_segment(&mut reader)? else {
            break;
        };
        if !terminated {
            break;
        }
        line.pop();
        let index = records.len();
        let record = AuditRecord::from_canonical_line(&line).map_err(|error| {
            LedgerError::Integrity(format!("line {} is invalid: {error}", index + 1))
        })?;
        let expected_sequence = u64::try_from(index).expect("record count fits u64") + 1;
        let expected_previous = records
            .last()
            .map_or(GENESIS_PREVIOUS_HASH, AuditRecord::hash);
        if record.sequence() != expected_sequence
            || record.previous_hash() != expected_previous
            || record.run_id() != run_id
        {
            return Err(LedgerError::Integrity(format!(
                "line {} does not continue this run's durable chain",
                index + 1
            )));
        }
        records.push(record);
    }
    if u64::try_from(records.len()).unwrap_or(u64::MAX) < head {
        return Err(LedgerError::Integrity(format!(
            "durable ledger holds {} records below the committed head {head}",
            records.len()
        )));
    }
    if require_complete && !reader.fill_buf().map_err(LedgerError::Io)?.is_empty() {
        return Err(LedgerError::Integrity(
            "operation history has an unpublished tail; authoritative recovery is required"
                .to_owned(),
        ));
    }
    if pin
        .manifest
        .as_ref()
        .is_some_and(|manifest| head >= manifest.durable_watermark)
    {
        crate::ledger_generation::validate_checkpoint(root, &pin, &records)?;
    }
    Ok(records)
}

struct GenerationReader {
    files: Vec<BufReader<File>>,
    current: usize,
}
impl std::io::Read for GenerationReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let available = self.fill_buf()?;
        let n = bytes.len().min(available.len());
        bytes[..n].copy_from_slice(&available[..n]);
        self.consume(n);
        Ok(n)
    }
}
impl BufRead for GenerationReader {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        while self.current < self.files.len() {
            if !self.files[self.current].fill_buf()?.is_empty() {
                return self.files[self.current].fill_buf();
            }
            self.current += 1;
        }
        Ok(&[])
    }
    fn consume(&mut self, amount: usize) {
        if let Some(reader) = self.files.get_mut(self.current) {
            reader.consume(amount);
        }
    }
}

fn is_reasoning_message(method: &str, raw: &[u8]) -> bool {
    if method.starts_with("item/reasoning/")
        || method.starts_with("item/plan/")
        || method.starts_with("turn/plan/")
    {
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
    matches!(
        lossless_path_string(&value, &["params", "item", "type"]).as_deref(),
        Some("reasoning" | "plan")
    )
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

fn validate_projection(projection: &RunStateProjection) -> Result<(), LedgerError> {
    crate::projection::validate(projection).map_err(LedgerError::Projection)
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

fn io_at(operation: &'static str, path: &Path, source: std::io::Error) -> LedgerError {
    LedgerError::IoContext {
        operation,
        path: path.to_path_buf(),
        source,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::is_microsecond_utc_timestamp;

    #[test]
    fn display_capture_motion_preserves_intact_prefix_and_reports_capture_conflict() {
        assert_eq!(
            injected_display_capture_motion(None).code,
            "RUN_STATE_CONFLICT"
        );
    }

    #[test]
    fn display_capture_motion_does_not_hide_final_fence_prefix_or_security_damage() {
        for damage in ["corrupt", "truncate", "security", "rewrite"] {
            assert_eq!(
                injected_display_capture_motion(Some(damage)).code,
                "AUDIT_INTEGRITY_FAILURE",
                "{damage}"
            );
        }
    }

    fn injected_display_capture_motion(
        damage: Option<&'static str>,
    ) -> crate::machine::MachineError {
        use std::cell::Cell;
        use std::os::unix::fs::DirBuilderExt as _;
        use std::rc::Rc;
        struct OwnedRoot(PathBuf);
        impl Drop for OwnedRoot {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        struct FenceGuard;
        impl Drop for FenceGuard {
            fn drop(&mut self) {
                DISPLAY_CAPTURE_FENCE_HOOK.with(|hook| *hook.borrow_mut() = None);
            }
        }
        let mounted = std::process::Command::new("/sbin/mount")
            .output()
            .is_ok_and(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .any(|line| line.contains(" on /Volumes/RootKernel ("))
            });
        let name = format!("dolgorae-capture-motion-{}", Uuid::now_v7());
        let preferred = Path::new("/Volumes/RootKernel/tmp").join(&name);
        let owned = if mounted
            && fs::create_dir_all("/Volumes/RootKernel/tmp").is_ok()
            && fs::DirBuilder::new().mode(0o700).create(&preferred).is_ok()
        {
            OwnedRoot(preferred)
        } else {
            let root = std::env::temp_dir().join(name);
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            OwnedRoot(root)
        };
        let run_id = Uuid::now_v7();
        let runs = owned.0.join("runs");
        let root = runs.join(run_id.to_string());
        fs::DirBuilder::new().mode(0o700).create(&runs).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let path = root.join("audit.jsonl");
        let first = AuditRecord::new(
            1,
            "2026-10-14T12:00:00.000000Z",
            run_id,
            1,
            AuditKind::RunCreated,
            parse("{}").unwrap(),
            GENESIS_PREVIOUS_HASH,
        )
        .unwrap();
        let second = AuditRecord::new(
            2,
            "2026-10-14T12:00:00.000001Z",
            run_id,
            1,
            AuditKind::ThreadBound,
            parse(r#"{"thread_id":"published-next"}"#).unwrap(),
            first.hash(),
        )
        .unwrap();
        let rewritten = AuditRecord::new(
            1,
            "2026-10-14T12:00:00.000000Z",
            run_id,
            1,
            AuditKind::RunCreated,
            parse(r#"{"tampered":1}"#).unwrap(),
            GENESIS_PREVIOUS_HASH,
        )
        .unwrap()
        .canonical_line()
        .unwrap();
        let first_bytes = first.canonical_line().unwrap();
        let next_bytes = second.canonical_line().unwrap();
        let mut writer = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        writer.write_all(&first_bytes).unwrap();
        writer.sync_all().unwrap();
        ObservedLedger::open_display(&root, run_id, 1).unwrap();
        // Record 1 remains the fixed published head. Ordinary record 2 is
        // appended in four valid canonical-byte fragments and is not published.
        writer.write_all(&next_bytes[..1]).unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        let split = next_bytes.len() / 3;
        let chunks = [
            next_bytes[1..split].to_vec(),
            next_bytes[split..2 * split].to_vec(),
            next_bytes[2 * split..].to_vec(),
        ];
        let crossings = Rc::new(Cell::new(0));
        let observed_crossings = Rc::clone(&crossings);
        DISPLAY_CAPTURE_FENCE_HOOK.with(|hook| {
            *hook.borrow_mut() = Some((
                run_id,
                Box::new(move || {
                    let index = observed_crossings.get();
                    assert!(
                        index < chunks.len(),
                        "capture exceeded its existing three fences"
                    );
                    let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
                    writer.write_all(&chunks[index]).unwrap();
                    writer.sync_all().unwrap();
                    drop(writer);
                    if index == 2 {
                        match damage {
                            Some("corrupt") => {
                                let mut file = OpenOptions::new().write(true).open(&path).unwrap();
                                file.write_all(b"!").unwrap();
                                file.sync_all().unwrap();
                            }
                            Some("truncate") => {
                                let file = OpenOptions::new().write(true).open(&path).unwrap();
                                file.set_len(0).unwrap();
                                file.sync_all().unwrap();
                            }
                            Some("security") => {
                                use std::os::unix::fs::PermissionsExt as _;
                                fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
                                    .unwrap();
                            }
                            Some("rewrite") => {
                                fs::write(&path, &rewritten).unwrap();
                            }
                            None => {}
                            _ => unreachable!("closed test damage set"),
                        }
                    }
                    observed_crossings.set(index + 1);
                }),
            ));
        });
        let guard = FenceGuard;
        let result = ObservedLedger::open_run_display(&owned.0, run_id, 1)
            .map_err(|error| display_observation_error(run_id, 1, RunLifecycle::Starting, error));
        drop(guard);
        assert_eq!(crossings.get(), 3);
        if damage.is_none() {
            let mut fresh = Vec::new();
            ObservedLedger::open(&root, run_id, 1)
                .unwrap()
                .write_canonical_history(&mut fresh)
                .unwrap();
            assert_eq!(fresh, first_bytes);
            assert!(matches!(
                ObservedLedger::open_complete(&root, run_id, 1),
                Err(LedgerError::Integrity(_))
            ));
            let mut stable = Vec::new();
            ObservedLedger::open_display(&root, run_id, 1)
                .unwrap()
                .write_canonical_history(&mut stable)
                .unwrap();
            assert_eq!(stable, fresh);
        }
        result.unwrap_err()
    }

    #[test]
    fn derived_observations_share_one_verified_head_without_crossing_an_older_boundary() {
        let run_id = Uuid::now_v7();
        let mut records = Vec::new();
        for (kind, value) in [
            (AuditKind::RunCreated, serde_json::json!({})),
            (
                AuditKind::ThreadBound,
                serde_json::json!({"thread_id":"thread", "thread_generation":1}),
            ),
            (
                AuditKind::TurnTerminal,
                serde_json::json!({"turn_id":"turn", "status":"completed"}),
            ),
        ] {
            let record = AuditRecord::new(
                records.len() as u64 + 1,
                "2026-10-14T12:00:00.000000Z",
                run_id,
                1,
                kind,
                parse(&value.to_string()).unwrap(),
                records
                    .last()
                    .map_or(GENESIS_PREVIOUS_HASH, AuditRecord::hash),
            )
            .unwrap();
            records.push(record);
        }
        let observed = ObservedLedger {
            records: Arc::new(records),
            head: 3,
            derived: Arc::new(ObservedLedgerDerived::default()),
        };
        let cloned = observed.clone();
        let first = observed.policy_inputs().unwrap();
        assert!(Arc::ptr_eq(&first, &cloned.policy_inputs().unwrap()));
        assert_eq!(
            observed.last_terminal().unwrap().unwrap()["turn_id"],
            "turn"
        );
        assert!(cloned.derived.terminal.get().is_some());
        let older = ObservedLedger {
            records: Arc::clone(&observed.records),
            head: 2,
            derived: Arc::new(ObservedLedgerDerived::default()),
        };
        assert!(older.last_terminal().unwrap().is_none());
        assert_eq!(older.policy_inputs().unwrap().bindings.len(), 1);
        assert_eq!(cloned.last_terminal().unwrap().unwrap()["turn_id"], "turn");
    }

    #[test]
    fn production_timestamp_formatter_emits_calendar_valid_microsecond_utc() {
        let cases = [
            (UNIX_EPOCH, "1970-01-01T00:00:00.000000Z"),
            (
                UNIX_EPOCH + Duration::from_secs(951_782_400),
                "2000-02-29T00:00:00.000000Z",
            ),
            (
                UNIX_EPOCH + Duration::from_secs(4_102_444_800),
                "2100-01-01T00:00:00.000000Z",
            ),
        ];
        for (instant, expected) in cases {
            let timestamp = format_system_timestamp(instant);
            assert_eq!(timestamp, expected);
            assert!(is_microsecond_utc_timestamp(&timestamp));
        }
    }

    #[test]
    fn append_capacity_accepts_exact_limits_and_rejects_the_first_excess() {
        assert!(check_append_capacity(MAX_LEDGER_BYTES - 1, MAX_LEDGER_RECORDS - 1, 1).is_ok());
        assert!(check_append_capacity(MAX_LEDGER_BYTES, MAX_LEDGER_RECORDS - 1, 1).is_err());
        assert!(check_append_capacity(0, MAX_LEDGER_RECORDS, 1).is_err());
    }
    #[test]
    fn ordinary_exhaustion_preserves_terminal_close_and_cleanup_appends() {
        use std::os::unix::fs::DirBuilderExt as _;
        let root = std::env::temp_dir().join(format!("dolgorae-reserve-{}", Uuid::now_v7()));
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
        let run_id = Uuid::now_v7();
        for (records_full, pending_count) in [(false, 0), (true, 0), (false, 8), (true, 8)] {
            let mut ledger = Ledger::open(&root, run_id).unwrap();
            ledger
                .append_required_payload(
                    AuditKind::ThreadBound,
                    &serde_json::json!({"thread_id":"retained"}),
                    1,
                )
                .unwrap();
            ledger
                .append_required_payload(
                    AuditKind::TurnStarted,
                    &serde_json::json!({"turn_id":"accepted"}),
                    1,
                )
                .unwrap();
            for id in 0..pending_count {
                ledger
                    .append_required_payload(
                        AuditKind::InteractionOpened,
                        &serde_json::json!({"request_id":id.to_string()}),
                        1,
                    )
                    .unwrap();
            }
            // Exercise byte and record accounting independently at their real admission boundaries.
            if records_full {
                ledger.generation_records =
                    MAX_LEDGER_RECORDS - FINALIZATION_RESERVED_RECORDS - pending_count * 2;
            } else {
                ledger.ledger_bytes = MAX_LEDGER_BYTES
                    - FINALIZATION_RESERVED_BYTES
                    - (pending_count * 2 * MAX_AUDIT_LINE_BYTES) as u64;
            }
            let before = fs::read(root.join("audit.jsonl")).unwrap();
            // This is the never-policy journal path: a fresh Opened followed by
            // Resolved, without an ApprovalRequested. Repetition must not drain reserve.
            for request_id in 100..400 {
                assert!(matches!(
                    ledger.append_required_payload(
                        AuditKind::InteractionOpened,
                        &serde_json::json!({"request_id": request_id.to_string(),
                        "method":"item/commandExecution/requestApproval"}),
                        1
                    ),
                    Err(LedgerError::Capacity(_))
                ));
            }
            assert_eq!(fs::read(root.join("audit.jsonl")).unwrap(), before);
            let effect = std::sync::atomic::AtomicBool::new(false);
            let ordinary = AuditRecord::new(
                ledger.next_sequence(),
                ledger.clock.timestamp(),
                run_id,
                1,
                AuditKind::TurnIntent,
                parse("{}").unwrap(),
                ledger.previous_hash(),
            )
            .unwrap();
            let refused = ledger.append_before_effect(ordinary, || {
                effect.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            });
            assert!(matches!(refused, Err(LedgerError::Capacity(_))));
            assert!(!effect.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(fs::read(root.join("audit.jsonl")).unwrap(), before);
            for id in 0..pending_count {
                ledger.append_required_payload(AuditKind::ApprovalDecided, &serde_json::json!({"request_id":id.to_string(),"resolution":{"outcome":"stale"}}), 1).unwrap();
                ledger
                    .append_required_payload(
                        AuditKind::InteractionResolved,
                        &serde_json::json!({"request_id":id.to_string()}),
                        1,
                    )
                    .unwrap();
            }
            for (kind, payload) in [
                (
                    AuditKind::TurnTerminal,
                    serde_json::json!({"turn_id":"accepted"}),
                ),
                (
                    AuditKind::MutationAdmitted,
                    serde_json::json!({"operation":"close"}),
                ),
                (AuditKind::WriterReleased, serde_json::json!({})),
                (
                    AuditKind::CleanupResult,
                    serde_json::json!({"confirmed":true}),
                ),
                (
                    AuditKind::LifecycleTransition,
                    serde_json::json!({"current":"closed"}),
                ),
            ] {
                let record = AuditRecord::new(
                    ledger.next_sequence(),
                    ledger.clock.timestamp(),
                    run_id,
                    1,
                    kind,
                    parse(&payload.to_string()).unwrap(),
                    ledger.previous_hash(),
                )
                .unwrap();
                if matches!(
                    kind,
                    AuditKind::CleanupResult | AuditKind::LifecycleTransition
                ) {
                    ledger
                        .append_conformance_record(record, AppendDurability::Required)
                        .unwrap();
                } else {
                    ledger.append(record, AppendDurability::Required).unwrap();
                }
            }
            let projection = ledger.projection().unwrap();
            assert_eq!(projection.lifecycle, RunLifecycle::Closed);
            assert!(projection.pending_requests.is_empty());
            drop(ledger);
            fs::write(root.join("audit.jsonl"), b"").unwrap();
            fs::remove_file(root.join("state.json")).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }
}
