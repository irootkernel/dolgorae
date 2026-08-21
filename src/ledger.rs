use crate::audit::{AuditError, AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH};
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
pub const MAX_LEDGER_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_LEDGER_RECORDS: usize = 1_000_000;
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
    InvalidRecord(String),
    InvalidEvent(String),
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
            Self::Integrity(reason) => write!(formatter, "audit integrity failure: {reason}"),
            Self::InvalidRecord(reason) => write!(formatter, "invalid audit record: {reason}"),
            Self::InvalidEvent(reason) => write!(formatter, "invalid client event: {reason}"),
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
    durable_len: usize,
    projection: RunStateProjection,
    tail_projection: RunStateProjection,
    pending_since_millis: Option<u64>,
    poisoned: bool,
    clock: Arc<C>,
    replay_started_millis: u64,
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
            .open(&audit_path)
            .map_err(|error| io_at("open audit ledger", &audit_path, error))?;
        DarwinSystem
            .lock_exclusive_nonblocking(&file)
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
        let (mut records, tail) =
            scan_ledger(&mut file, &audit_path, run_id, &clock, replay_started)?;
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
            pending_since_millis: None,
            poisoned: false,
            clock,
            replay_started_millis: replay_started,
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
        let line = record.canonical_line()?;
        if line.len() > MAX_AUDIT_LINE_BYTES {
            return Err(LedgerError::InvalidRecord(
                "canonical audit line exceeds the bounded record limit".to_owned(),
            ));
        }
        let line_bytes = u64::try_from(line.len()).unwrap_or(u64::MAX);
        check_append_capacity(self.ledger_bytes, self.records.len(), line_bytes)?;
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
        self.records.push(record);
        self.ledger_bytes = self.ledger_bytes.saturating_add(line_bytes);
        self.tail_projection = candidate_projection.clone();
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
        let mut deliveries = Vec::new();
        let mut delivery_bytes = 0_usize;
        let durable_records = self.durable_records()?;
        let first = durable_records.partition_point(|record| record.sequence() <= after);
        for record in &durable_records[first..] {
            if record.kind() != AuditKind::ClientEvent {
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

fn scan_ledger<C: LedgerClock>(
    file: &mut File,
    audit_path: &Path,
    run_id: Uuid,
    clock: &C,
    started_millis: u64,
) -> Result<(Vec<AuditRecord>, Option<TornTail>), LedgerError> {
    let ledger_bytes = file
        .metadata()
        .map_err(|error| io_at("inspect audit ledger", audit_path, error))?
        .len();
    if ledger_bytes > MAX_LEDGER_BYTES {
        return Err(LedgerError::Integrity(format!(
            "audit ledger exceeds the {MAX_LEDGER_BYTES}-byte replay bound"
        )));
    }
    let mut reader = BufReader::new(file.try_clone()?);
    reader.seek(SeekFrom::Start(0))?;
    let mut records = Vec::new();
    let mut prefix_length = 0_u64;
    let mut tail = None;
    while let Some((mut line, terminated)) = read_bounded_segment(&mut reader)? {
        if clock.monotonic_millis().saturating_sub(started_millis) >= REPLAY_BUDGET_MILLIS {
            return Err(LedgerError::OperationTimeout(format!(
                "full replay exceeded {REPLAY_BUDGET_MILLIS} milliseconds"
            )));
        }
        if records.len() >= MAX_LEDGER_RECORDS {
            return Err(LedgerError::Integrity(format!(
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

fn check_append_capacity(
    current_bytes: u64,
    current_records: usize,
    line_bytes: u64,
) -> Result<(), LedgerError> {
    if current_records >= MAX_LEDGER_RECORDS {
        return Err(LedgerError::Integrity(format!(
            "append would exceed the {MAX_LEDGER_RECORDS}-record ledger bound"
        )));
    }
    if current_bytes.saturating_add(line_bytes) > MAX_LEDGER_BYTES {
        return Err(LedgerError::Integrity(format!(
            "append would exceed the {MAX_LEDGER_BYTES}-byte ledger bound"
        )));
    }
    Ok(())
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
}
