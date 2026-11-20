use crate::domain::RunLifecycle;
use crate::machine::MachineError;
use crate::run::ControllerBinding;
use crate::workspace::{verify_secure_directory, verify_secure_file};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const RECORD_NAME: &str = "writer.json";
const LOCK_NAME: &str = "writer.lock";
const HANDOFF_LOCK_NAME: &str = "handoff.lock";
const SERVER_EPOCH_NAME: &str = "server-epoch";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriterAuthorityState {
    None,
    Reserved,
    Active,
    HandoffPrepared,
    Releasing,
    BlockedUnknown,
}

impl WriterAuthorityState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Reserved => "reserved",
            Self::Active => "active",
            Self::HandoffPrepared => "handoff_prepared",
            Self::Releasing => "releasing",
            Self::BlockedUnknown => "blocked_unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterHolder {
    pub run_id: Uuid,
    pub profile: String,
    pub controller_id: Uuid,
    pub controller_generation: u64,
    pub run_generation: u64,
    pub worker_generation: u64,
    pub profile_server_key: String,
    pub profile_server_epoch: u64,
    pub thread_id: Option<String>,
    pub lifecycle: RunLifecycle,
    #[serde(default)]
    pub active_turn_id: Option<String>,
    #[serde(default)]
    pub pending_interaction_count: usize,
    #[serde(default)]
    pub last_event_cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterHandoff {
    pub handoff_id: Uuid,
    pub source_run_id: Uuid,
    pub destination_run_id: Uuid,
    pub expected_writer_generation: u64,
    pub expires_at_unix_seconds: u64,
    pub controller_id: Uuid,
    pub controller_generation: u64,
    #[serde(default)]
    pub source_retirement_started: bool,
    #[serde(default)]
    pub source_retired: bool,
}

/// Private release authority. The public Writer projection deliberately omits it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GenerationReleasePhase {
    Prepared,
    StopStarted,
    OldAbsent,
    ReaderStarting,
    ReaderVerified,
    Committed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GenerationRetirementPurpose {
    #[default]
    Release,
    Acquire,
}

impl GenerationRetirementPurpose {
    fn is_release(&self) -> bool {
        *self == Self::Release
    }

    pub(crate) fn guarded_state(self) -> WriterAuthorityState {
        match self {
            Self::Release => WriterAuthorityState::Releasing,
            Self::Acquire => WriterAuthorityState::Reserved,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationRelease {
    #[serde(
        default,
        skip_serializing_if = "GenerationRetirementPurpose::is_release"
    )]
    pub purpose: GenerationRetirementPurpose,
    pub transaction_id: Uuid,
    pub source: WriterHolder,
    pub writer_generation: u64,
    pub authority_revision: u64,
    pub source_revision: u64,
    pub source_ledger_sha256: String,
    pub source_runtime_sha256: String,
    pub registration_sha256: String,
    pub scope_sha256: Option<String>,
    pub absence_sha256: Option<String>,
    pub reader_proof_sha256: Option<String>,
    pub retired_reader_sha256: Option<String>,
    pub phase: GenerationReleasePhase,
    pub successor: Option<ReaderReservation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReaderReservation {
    pub run_generation: u64,
    pub server_epoch: u64,
    pub control_socket_epoch: u64,
    pub attempt_started: bool,
}

impl GenerationRelease {
    pub(crate) fn pending(&self) -> bool {
        self.phase != GenerationReleasePhase::Committed
    }

    pub(crate) fn verify_ledger_prefix(
        &self,
        records: &[crate::audit::AuditRecord],
    ) -> Result<(), MachineError> {
        use crate::audit::AuditKind as K;
        let prefix = records
            .iter()
            .find(|r| r.sequence() == self.source_revision)
            .ok_or_else(|| invariant("release source prefix missing"))?;
        let head = crate::projection::LedgerHead {
            sequence: prefix.sequence(),
            hash: prefix.hash().to_owned(),
        };
        if crate::jcs::sha256_hex(
            &serde_json::to_vec(&head).map_err(|_| invariant("release head encoding"))?,
        ) != self.source_ledger_sha256
        {
            return Err(invariant("release source head changed"));
        }
        for record in records
            .iter()
            .filter(|r| r.sequence() > self.source_revision)
        {
            if !matches!(
                record.kind(),
                K::ControllerReset
                    | K::RunGenerationStarted
                    | K::RunGenerationStopped
                    | K::TurnIntent
                    | K::ThreadBound
                    | K::ProfileObserved
                    | K::AppServerRequest
                    | K::AppServerResponse
                    | K::AppServerNotification
                    | K::ClientEvent
                    | K::CleanupIntent
                    | K::CleanupResult
                    | K::StartFailed
            ) {
                return Err(invariant(
                    "unowned ledger advance during generation release",
                ));
            }
            if matches!(record.kind(), K::TurnIntent | K::ThreadBound) {
                let payload: Value = serde_json::from_slice(
                    &crate::jcs::canonicalize(record.payload())
                        .map_err(|_| invariant("release Thread binding encoding"))?,
                )
                .map_err(|_| invariant("release Thread binding encoding"))?;
                let reader_generation = self.successor.as_ref().is_some_and(|reservation| {
                    record.run_generation() > self.source.run_generation
                        && (record.run_generation() == reservation.run_generation
                            && reservation.attempt_started
                            || record.run_generation() < reservation.run_generation
                                && self.retired_reader_sha256.is_some())
                });
                let provisional = record.kind() == K::TurnIntent;
                if !matches!(
                    self.phase,
                    GenerationReleasePhase::ReaderStarting | GenerationReleasePhase::ReaderVerified
                ) || !reader_generation
                    || (provisional && payload.as_object().is_none_or(|fields| fields.len() != 2))
                    || payload.get("operation_id").and_then(Value::as_str)
                        != Some(self.transaction_id.to_string().as_str())
                    || payload
                        .get(if provisional {
                            "provisional_thread_id"
                        } else {
                            "thread_id"
                        })
                        .and_then(Value::as_str)
                        != self.source.thread_id.as_deref()
                {
                    return Err(invariant("release Thread binding names another operation"));
                }
            }
        }
        Ok(())
    }

    fn validate(&self) -> bool {
        use GenerationReleasePhase as P;
        let digest =
            |value: &str| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit());
        self.transaction_id.get_version_num() == 7
            && self.writer_generation > 0
            && self.source_revision > 0
            && self.source.active_turn_id.is_none()
            && self.source.pending_interaction_count == 0
            && matches!(
                self.source.lifecycle,
                RunLifecycle::Idle | RunLifecycle::Paused
            )
            && self
                .source
                .thread_id
                .as_ref()
                .is_some_and(|v| !v.is_empty())
            && digest(&self.source_ledger_sha256)
            && digest(&self.source_runtime_sha256)
            && digest(&self.registration_sha256)
            && self.scope_sha256.as_deref().is_none_or(digest)
            && self.absence_sha256.as_deref().is_none_or(digest)
            && self.reader_proof_sha256.as_deref().is_none_or(digest)
            && self.retired_reader_sha256.as_deref().is_none_or(digest)
            && (self.phase == P::Prepared || self.scope_sha256.is_some())
            && (!matches!(
                self.phase,
                P::OldAbsent | P::ReaderStarting | P::ReaderVerified | P::Committed
            ) || self.absence_sha256.is_some())
            && (!matches!(
                self.phase,
                P::ReaderStarting | P::ReaderVerified | P::Committed
            ) || self.successor.is_some())
            && (!matches!(self.phase, P::ReaderVerified | P::Committed)
                || self.reader_proof_sha256.is_some())
            && self.successor.as_ref().is_none_or(|r| {
                r.run_generation > self.source.run_generation
                    && r.server_epoch > self.source.profile_server_epoch
                    && r.control_socket_epoch > 0
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterRecord {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) generation_release: Option<GenerationRelease>,
    pub workspace_id: String,
    pub authority_revision: u64,
    pub state: WriterAuthorityState,
    pub writer_generation: u64,
    pub transaction_id: Option<Uuid>,
    pub holder: Option<WriterHolder>,
    pub handoff: Option<WriterHandoff>,
    #[serde(default)]
    pub cancelled_handoff: Option<WriterHandoff>,
    pub recovery_action: Option<String>,
    pub writer_lock_device: u64,
    pub writer_lock_inode: u64,
    pub handoff_lock_device: u64,
    pub handoff_lock_inode: u64,
}

impl WriterRecord {
    #[must_use]
    pub fn empty(workspace_id: &str) -> Self {
        Self {
            schema_version: 1,
            generation_release: None,
            workspace_id: workspace_id.to_owned(),
            authority_revision: 0,
            state: WriterAuthorityState::None,
            writer_generation: 0,
            transaction_id: None,
            holder: None,
            handoff: None,
            cancelled_handoff: None,
            recovery_action: None,
            writer_lock_device: 0,
            writer_lock_inode: 0,
            handoff_lock_device: 0,
            handoff_lock_inode: 0,
        }
    }

    fn initialized(workspace_id: &str, writer: &fs::Metadata, handoff: &fs::Metadata) -> Self {
        let mut record = Self::empty(workspace_id);
        record.writer_lock_device = writer.dev();
        record.writer_lock_inode = writer.ino();
        record.handoff_lock_device = handoff.dev();
        record.handoff_lock_inode = handoff.ino();
        record
    }

    fn validate(&self, workspace_id: &str) -> Result<(), MachineError> {
        let holder_required = self.state != WriterAuthorityState::None;
        let transaction_required = matches!(
            self.state,
            WriterAuthorityState::Reserved
                | WriterAuthorityState::Releasing
                | WriterAuthorityState::HandoffPrepared
                | WriterAuthorityState::BlockedUnknown
        );
        if !matches!(self.schema_version, 1 | 2)
            || (self.schema_version == 1 && self.generation_release.is_some())
            || (self.schema_version == 2 && self.generation_release.is_none())
            || self.generation_release.as_ref().is_some_and(|release| {
                !release.validate()
                    || (release.purpose == GenerationRetirementPurpose::Acquire
                        && release.phase == GenerationReleasePhase::Committed
                        && match self.state {
                            WriterAuthorityState::None => {
                                self.writer_generation != release.writer_generation
                            }
                            WriterAuthorityState::Reserved
                            | WriterAuthorityState::BlockedUnknown => self
                                .acquire_retirement_recovery(release.source.run_id)
                                .is_none(),
                            _ => true,
                        })
                    || (release.pending()
                        && (self.transaction_id != Some(release.transaction_id)
                            || self.holder.as_ref() != Some(&release.source)
                            || self.writer_generation != release.writer_generation
                            || (self.state != release.purpose.guarded_state()
                                && self.state != WriterAuthorityState::BlockedUnknown)))
            })
            || self.workspace_id != workspace_id
            || self.workspace_id.len() != 64
            || !self
                .workspace_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || holder_required != self.holder.is_some()
            || transaction_required != self.transaction_id.is_some()
            || (self.state == WriterAuthorityState::HandoffPrepared) != self.handoff.is_some()
            || (self.state == WriterAuthorityState::None
                && self.writer_generation != 0
                && self.holder.is_some())
            || self.writer_lock_device == 0
            || self.writer_lock_inode == 0
            || self.handoff_lock_device == 0
            || self.handoff_lock_inode == 0
        {
            return Err(invariant("writer authority record is inconsistent"));
        }
        if let Some(holder) = &self.holder
            && (holder.run_id.get_version_num() != 7
                || holder.controller_id.get_version_num() != 7
                || holder.controller_generation == 0
                || holder.run_generation == 0
                || holder.worker_generation == 0
                || holder.profile_server_epoch == 0
                || holder.profile_server_key.len() != 64)
        {
            return Err(invariant("writer holder identity is invalid"));
        }
        Ok(())
    }

    pub(crate) fn pending_generation_release(&self, run_id: Uuid) -> Option<&GenerationRelease> {
        self.generation_release
            .as_ref()
            .filter(|r| r.pending() && r.source.run_id == run_id)
    }

    pub(crate) fn acquire_retirement_recovery(&self, run_id: Uuid) -> Option<&GenerationRelease> {
        self.generation_release.as_ref().filter(|owner| {
            owner.purpose == GenerationRetirementPurpose::Acquire
                && owner.phase == GenerationReleasePhase::Committed
                && owner.source.run_id == run_id
                && self.transaction_id == Some(owner.transaction_id)
                && self.writer_generation == owner.writer_generation
                && matches!(
                    self.state,
                    WriterAuthorityState::Reserved | WriterAuthorityState::BlockedUnknown
                )
                && self
                    .holder
                    .as_ref()
                    .zip(owner.successor.as_ref())
                    .is_some_and(|(holder, reader)| {
                        reader.attempt_started
                            && reader.run_generation > owner.source.run_generation
                            && reader.server_epoch > owner.source.profile_server_epoch
                            && reader.control_socket_epoch > 0
                            && holder.run_id == run_id
                            && holder.controller_id == owner.source.controller_id
                            && holder.controller_generation == owner.source.controller_generation
                            && holder.profile == owner.source.profile
                            && holder.profile_server_key == owner.source.profile_server_key
                            && holder.run_generation == reader.run_generation
                            && holder.worker_generation == reader.run_generation
                            && holder.profile_server_epoch == reader.server_epoch
                            && holder.thread_id == owner.source.thread_id
                    })
        })
    }

    pub(crate) fn prepare_acquire_retirement_recovery(
        &mut self,
        run_id: Uuid,
    ) -> Result<(), MachineError> {
        if self.acquire_retirement_recovery(run_id).is_none() {
            return Err(invariant(
                "latest acquire generation is not a recoverable reservation",
            ));
        }
        self.state = WriterAuthorityState::Reserved;
        Ok(())
    }

    pub(crate) fn require_no_generation_release(&self, run_id: Uuid) -> Result<(), MachineError> {
        if self.pending_generation_release(run_id).is_some()
            || self.acquire_retirement_recovery(run_id).is_some()
        {
            return Err(MachineError::new(
                "RECOVERY_REQUIRED",
                "the original generation release must be resumed",
                false,
                json!({"run_id":run_id,"required_action":"reconcile_generation_release"}),
            ));
        }
        Ok(())
    }

    pub(crate) fn prepare_generation_release(
        &mut self,
        release: GenerationRelease,
    ) -> Result<(), MachineError> {
        if self.state != release.purpose.guarded_state()
            || self.transaction_id != Some(release.transaction_id)
            || self.holder.as_ref() != Some(&release.source)
            || self.writer_generation != release.writer_generation
            || self
                .generation_release
                .as_ref()
                .is_some_and(GenerationRelease::pending)
            || !release.validate()
            || release.phase != GenerationReleasePhase::Prepared
        {
            return Err(invariant("generation release admission changed"));
        }
        self.schema_version = 2;
        self.generation_release = Some(release);
        Ok(())
    }

    pub(crate) fn retain_prepared_scope(
        &mut self,
        expected: &GenerationRelease,
        digest: String,
    ) -> Result<(), MachineError> {
        if self.generation_release.as_ref() != Some(expected)
            || self.transaction_id != Some(expected.transaction_id)
            || expected.phase != GenerationReleasePhase::Prepared
            || expected
                .scope_sha256
                .as_ref()
                .is_some_and(|old| old != &digest)
        {
            return Err(invariant("release preparation scope changed"));
        }
        let mut next = expected.clone();
        next.scope_sha256 = Some(digest);
        if !next.validate() {
            return Err(invariant("release preparation scope is invalid"));
        }
        self.generation_release = Some(next);
        Ok(())
    }

    pub(crate) fn advance_generation_release(
        &mut self,
        expected: &GenerationRelease,
        next: GenerationRelease,
    ) -> Result<(), MachineError> {
        use GenerationReleasePhase as P;
        let legal = matches!(
            (expected.phase, next.phase),
            (P::Prepared, P::StopStarted)
                | (P::StopStarted, P::OldAbsent)
                | (P::OldAbsent, P::ReaderStarting)
                | (P::ReaderStarting, P::ReaderVerified)
                | (P::ReaderVerified, P::Committed)
        );
        let mut retained = next.clone();
        retained.phase = expected.phase;
        retained.scope_sha256 = expected.scope_sha256.clone();
        retained.absence_sha256 = expected.absence_sha256.clone();
        retained.reader_proof_sha256 = expected.reader_proof_sha256.clone();
        retained.successor = expected.successor.clone();
        if self.generation_release.as_ref() != Some(expected)
            || self.transaction_id != Some(expected.transaction_id)
            || !legal
            || !next.validate()
            || retained != *expected
            || (expected.scope_sha256.is_some() && next.scope_sha256 != expected.scope_sha256)
            || (expected.absence_sha256.is_some() && next.absence_sha256 != expected.absence_sha256)
            || (expected.successor.is_some() && next.successor != expected.successor)
        {
            return Err(invariant("generation release checkpoint changed"));
        }
        self.state = next.purpose.guarded_state();
        self.generation_release = Some(next);
        Ok(())
    }

    pub(crate) fn commit_generation_retirement(
        &mut self,
        expected: &GenerationRelease,
        retain_acquire: bool,
    ) -> Result<(), MachineError> {
        if expected.phase != GenerationReleasePhase::ReaderVerified
            || (retain_acquire && expected.purpose != GenerationRetirementPurpose::Acquire)
        {
            return Err(invariant("retirement commit purpose changed"));
        }
        let mut next = expected.clone();
        next.phase = GenerationReleasePhase::Committed;
        self.advance_generation_release(expected, next)?;
        if expected.purpose == GenerationRetirementPurpose::Acquire {
            if retain_acquire {
                let successor = expected
                    .successor
                    .as_ref()
                    .ok_or_else(|| invariant("acquire Reader reservation missing"))?;
                self.bind_reserved_generation(
                    expected.transaction_id,
                    successor.run_generation,
                    successor.server_epoch,
                )?;
            } else {
                self.cancel_acquire(expected.transaction_id);
            }
            Ok(())
        } else {
            self.commit_release(expected.transaction_id)
        }
    }

    pub(crate) fn replace_absent_reader(
        &mut self,
        expected: &GenerationRelease,
        reservation: ReaderReservation,
        evidence_sha256: String,
    ) -> Result<(), MachineError> {
        let old = expected
            .successor
            .as_ref()
            .ok_or_else(|| invariant("Reader reservation missing"))?;
        if self.generation_release.as_ref() != Some(expected)
            || self.transaction_id != Some(expected.transaction_id)
            || !matches!(
                expected.phase,
                GenerationReleasePhase::ReaderStarting | GenerationReleasePhase::ReaderVerified
            )
            || !old.attempt_started
            || reservation.attempt_started
            || reservation.run_generation <= old.run_generation
            || reservation.server_epoch <= old.server_epoch
            || reservation.control_socket_epoch <= old.control_socket_epoch
            || evidence_sha256.len() != 64
            || !evidence_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invariant(
                "Reader replacement lacks exact candidate absence",
            ));
        }
        let release = self.generation_release.as_mut().expect("checked owner");
        release.phase = GenerationReleasePhase::ReaderStarting;
        release.reader_proof_sha256 = None;
        release.retired_reader_sha256 = Some(evidence_sha256);
        release.successor = Some(reservation);
        Ok(())
    }

    pub fn prepare_acquire(
        &mut self,
        requested_run_id: Uuid,
        holder: WriterHolder,
        transaction_id: Uuid,
    ) -> Result<(bool, u64), MachineError> {
        if self.state == WriterAuthorityState::Active
            && self
                .holder
                .as_ref()
                .is_some_and(|current| current.run_id == requested_run_id)
        {
            return Ok((true, self.writer_generation));
        }
        if self.state != WriterAuthorityState::None {
            return Err(writer_busy(self, requested_run_id));
        }
        if self.generation_release.as_ref().is_some_and(|owner| {
            owner.purpose == GenerationRetirementPurpose::Acquire && !owner.pending()
        }) {
            // The previous Reader recovery completed. WriterStore preserves its
            // immutable witness before publishing this new reservation.
            self.generation_release = None;
            self.schema_version = 1;
        }
        let generation = self
            .writer_generation
            .checked_add(1)
            .ok_or_else(|| invariant("writer generation overflow"))?;
        self.state = WriterAuthorityState::Reserved;
        self.writer_generation = generation;
        self.transaction_id = Some(transaction_id);
        self.holder = Some(holder);
        self.handoff = None;
        self.recovery_action = None;
        Ok((false, generation))
    }

    pub fn commit_acquire(
        &mut self,
        transaction_id: Uuid,
        generation: u64,
    ) -> Result<(), MachineError> {
        if self.state != WriterAuthorityState::Reserved
            || self.transaction_id != Some(transaction_id)
            || self.writer_generation != generation
        {
            return Err(invariant("writer reservation changed before commit"));
        }
        if let Some(owner) = self.generation_release.as_ref()
            && owner.purpose == GenerationRetirementPurpose::Acquire
        {
            if self
                .acquire_retirement_recovery(owner.source.run_id)
                .is_none()
            {
                return Err(invariant(
                    "acquire retirement is not the verified current reservation",
                ));
            }
            // WriterStore archives the committed Reader witness before this
            // same atomic record publication consumes it and commits Active.
            self.generation_release = None;
            self.schema_version = 1;
        }
        self.state = WriterAuthorityState::Active;
        self.transaction_id = None;
        Ok(())
    }

    /// Complete an ambiguous acquire only after the caller service validates
    /// fresh, current qualified Write proof for this exact pending operation.
    pub(crate) fn commit_verified_recovered_acquire(
        &mut self,
        transaction_id: Uuid,
        generation: u64,
    ) -> Result<(), MachineError> {
        if self.state != WriterAuthorityState::BlockedUnknown
            || self.transaction_id != Some(transaction_id)
            || self.writer_generation != generation
        {
            return Err(invariant("writer recovery changed before acquire commit"));
        }
        self.require_no_generation_release(
            self.holder
                .as_ref()
                .ok_or_else(|| invariant("holder missing"))?
                .run_id,
        )?;
        self.state = WriterAuthorityState::Reserved;
        self.commit_acquire(transaction_id, generation)
    }

    pub fn bind_reserved_generation(
        &mut self,
        transaction_id: Uuid,
        worker_generation: u64,
        server_epoch: u64,
    ) -> Result<(), MachineError> {
        if self.state != WriterAuthorityState::Reserved
            || self.transaction_id != Some(transaction_id)
            || worker_generation == 0
            || server_epoch == 0
        {
            return Err(invariant("writer reservation changed before lane binding"));
        }
        let holder = self
            .holder
            .as_mut()
            .ok_or_else(|| invariant("writer reservation holder is missing"))?;
        holder.run_generation = worker_generation;
        holder.worker_generation = worker_generation;
        holder.profile_server_epoch = server_epoch;
        Ok(())
    }

    pub fn cancel_acquire(&mut self, transaction_id: Uuid) {
        if self.state == WriterAuthorityState::Reserved
            && self.transaction_id == Some(transaction_id)
        {
            self.state = WriterAuthorityState::None;
            self.transaction_id = None;
            self.holder = None;
            self.recovery_action = None;
        }
    }

    pub(crate) fn release_aborted_initial_acquire(
        &mut self,
        run_id: Uuid,
    ) -> Result<(), MachineError> {
        let Some(holder) = self
            .holder
            .as_ref()
            .filter(|holder| holder.run_id == run_id)
        else {
            // An earlier attempt may already have settled before a new Run acquired.
            if self.transaction_id == Some(run_id) {
                return Err(invariant(
                    "startup abort found its transaction without its holder",
                ));
            }
            return Ok(());
        };
        self.require_no_generation_release(run_id)?;
        if !matches!(
            self.state,
            WriterAuthorityState::Reserved | WriterAuthorityState::BlockedUnknown
        ) || self.transaction_id != Some(run_id)
            || holder.run_generation != 1
            || holder.worker_generation != 1
            || holder.lifecycle != RunLifecycle::Starting
            || holder.thread_id.is_some()
            || holder.active_turn_id.is_some()
        {
            return Err(invariant(
                "startup abort does not own this Writer reservation",
            ));
        }
        self.state = WriterAuthorityState::Reserved;
        self.cancel_acquire(run_id);
        Ok(())
    }

    pub fn block_unknown(&mut self, transaction_id: Uuid) {
        if self.transaction_id == Some(transaction_id) {
            self.state = WriterAuthorityState::BlockedUnknown;
            self.handoff = None;
            self.recovery_action = Some("reverify_writer_policy".to_owned());
        }
    }

    /// The caller service invokes this only after fresh qualified Reader proof
    /// or complete old-generation absence has been checked against this exact
    /// guarded transaction under the lock.
    pub(crate) fn retire_verified_reader_guard(
        &mut self,
        transaction_id: Uuid,
    ) -> Result<(), MachineError> {
        if !matches!(
            self.state,
            WriterAuthorityState::Reserved
                | WriterAuthorityState::Releasing
                | WriterAuthorityState::BlockedUnknown
        ) || self.transaction_id != Some(transaction_id)
        {
            return Err(invariant("writer recovery guard changed before retirement"));
        }
        self.require_no_generation_release(
            self.holder
                .as_ref()
                .ok_or_else(|| invariant("holder missing"))?
                .run_id,
        )?;
        self.state = WriterAuthorityState::Releasing;
        self.commit_release(transaction_id)
    }

    pub fn prepare_release(
        &mut self,
        requested_run_id: Uuid,
        transaction_id: Uuid,
    ) -> Result<Option<u64>, MachineError> {
        if self.state == WriterAuthorityState::None {
            return Ok(None);
        }
        if self.state != WriterAuthorityState::Active
            || self
                .holder
                .as_ref()
                .is_none_or(|holder| holder.run_id != requested_run_id)
        {
            return Err(writer_busy(self, requested_run_id));
        }
        self.state = WriterAuthorityState::Releasing;
        self.transaction_id = Some(transaction_id);
        Ok(Some(self.writer_generation))
    }

    pub fn rollback_release(&mut self, transaction_id: Uuid) {
        if self.state == WriterAuthorityState::Releasing
            && self.transaction_id == Some(transaction_id)
            && self
                .generation_release
                .as_ref()
                .is_none_or(|r| !r.pending())
        {
            self.state = WriterAuthorityState::Active;
            self.transaction_id = None;
        }
    }

    pub fn commit_release(&mut self, transaction_id: Uuid) -> Result<(), MachineError> {
        if self.state != WriterAuthorityState::Releasing
            || self.transaction_id != Some(transaction_id)
        {
            return Err(invariant("writer release changed before commit"));
        }
        if self
            .generation_release
            .as_ref()
            .is_some_and(GenerationRelease::pending)
        {
            return Err(invariant("generation release lacks committed Reader proof"));
        }
        self.state = WriterAuthorityState::None;
        self.transaction_id = None;
        self.holder = None;
        self.handoff = None;
        self.recovery_action = None;
        Ok(())
    }

    pub fn prepare_handoff(&mut self, handoff: WriterHandoff) -> Result<(), MachineError> {
        if self.writer_generation != handoff.expected_writer_generation {
            return Err(MachineError::new(
                "STALE_WRITER_GENERATION",
                "writer generation changed before handoff prepare",
                false,
                json!({
                    "expected_generation": handoff.expected_writer_generation,
                    "actual_generation": self.writer_generation,
                }),
            ));
        }
        if self.state != WriterAuthorityState::Active
            || self
                .holder
                .as_ref()
                .is_none_or(|holder| holder.run_id != handoff.source_run_id)
        {
            return Err(writer_busy(self, handoff.destination_run_id));
        }
        self.state = WriterAuthorityState::HandoffPrepared;
        self.transaction_id = Some(handoff.handoff_id);
        self.handoff = Some(handoff);
        self.cancelled_handoff = None;
        Ok(())
    }

    pub fn cancel_handoff(&mut self, handoff_id: Uuid) -> Result<(), MachineError> {
        if self.cancelled_handoff.as_ref().is_some_and(|handoff| {
            handoff.handoff_id == handoff_id
                && handoff.expected_writer_generation == self.writer_generation
        }) {
            return Ok(());
        }
        if self.state != WriterAuthorityState::HandoffPrepared
            || self.transaction_id != Some(handoff_id)
        {
            return Err(invariant("writer handoff changed before cancellation"));
        }
        let (source_retirement_started, source_retired) =
            self.handoff.as_ref().map_or((false, false), |handoff| {
                (handoff.source_retirement_started, handoff.source_retired)
            });
        self.state = if source_retired {
            self.holder = None;
            WriterAuthorityState::None
        } else if source_retirement_started {
            self.recovery_action = Some("reverify_source_writer_policy".to_owned());
            WriterAuthorityState::BlockedUnknown
        } else {
            WriterAuthorityState::Active
        };
        if self.state != WriterAuthorityState::BlockedUnknown {
            self.transaction_id = None;
        }
        self.cancelled_handoff = self.handoff.take();
        Ok(())
    }

    pub fn begin_handoff_source_retirement(
        &mut self,
        handoff_id: Uuid,
    ) -> Result<(), MachineError> {
        if self.state != WriterAuthorityState::HandoffPrepared
            || self.transaction_id != Some(handoff_id)
        {
            return Err(invariant("writer handoff changed before source retirement"));
        }
        let handoff = self
            .handoff
            .as_mut()
            .ok_or_else(|| invariant("writer handoff is missing"))?;
        handoff.source_retirement_started = true;
        Ok(())
    }

    pub fn retire_handoff_source(&mut self, handoff_id: Uuid) -> Result<(), MachineError> {
        if self.state != WriterAuthorityState::HandoffPrepared
            || self.transaction_id != Some(handoff_id)
        {
            return Err(invariant("writer handoff changed before source retirement"));
        }
        let handoff = self
            .handoff
            .as_mut()
            .ok_or_else(|| invariant("writer handoff is missing"))?;
        handoff.source_retired = true;
        Ok(())
    }

    pub fn abort_handoff_source_retirement(
        &mut self,
        handoff_id: Uuid,
    ) -> Result<(), MachineError> {
        if self.state != WriterAuthorityState::HandoffPrepared
            || self.transaction_id != Some(handoff_id)
        {
            return Err(invariant("writer handoff changed before source rollback"));
        }
        let handoff = self
            .handoff
            .as_mut()
            .ok_or_else(|| invariant("writer handoff is missing"))?;
        if handoff.source_retired {
            return Err(invariant("retired source policy cannot be rolled back"));
        }
        handoff.source_retirement_started = false;
        Ok(())
    }

    pub fn reserve_handoff_destination(
        &mut self,
        handoff_id: Uuid,
        expected_generation: u64,
        holder: WriterHolder,
    ) -> Result<u64, MachineError> {
        if self.state != WriterAuthorityState::HandoffPrepared
            || self.transaction_id != Some(handoff_id)
            || self.writer_generation != expected_generation
        {
            return Err(invariant(
                "writer handoff changed before destination reservation",
            ));
        }
        let generation = expected_generation
            .checked_add(1)
            .ok_or_else(|| invariant("writer generation overflow"))?;
        self.state = WriterAuthorityState::Reserved;
        self.writer_generation = generation;
        self.holder = Some(holder);
        self.handoff = None;
        Ok(generation)
    }
}

pub struct WriterStore {
    state_root: PathBuf,
    workspace_id: String,
    uid: u32,
}

impl WriterStore {
    #[must_use]
    pub fn new(state_root: impl Into<PathBuf>, workspace_id: impl Into<String>, uid: u32) -> Self {
        Self {
            state_root: state_root.into(),
            workspace_id: workspace_id.into(),
            uid,
        }
    }

    pub fn initialize_layout(
        state_root: &Path,
        workspace_id: &str,
        uid: u32,
    ) -> Result<(), MachineError> {
        let runtime = state_root.join("runtime");
        let locks = runtime.join("locks");
        verify_secure_directory(&runtime, uid)?;
        verify_secure_directory(&locks, uid)?;
        let startup = locks.join("startup");
        if !startup.exists() {
            fs::create_dir(&startup).map_err(|error| path_error(&startup, error))?;
            fs::set_permissions(&startup, fs::Permissions::from_mode(0o700))
                .map_err(|error| path_error(&startup, error))?;
            sync_directory(&locks)?;
        }
        verify_secure_directory(&startup, uid)?;
        for name in [LOCK_NAME, HANDOFF_LOCK_NAME] {
            create_permanent_lock(&locks.join(name), uid)?;
        }
        let record = runtime.join(RECORD_NAME);
        if !record.exists() {
            let writer = fs::symlink_metadata(locks.join(LOCK_NAME))
                .map_err(|error| path_error(&locks.join(LOCK_NAME), error))?;
            let handoff = fs::symlink_metadata(locks.join(HANDOFF_LOCK_NAME))
                .map_err(|error| path_error(&locks.join(HANDOFF_LOCK_NAME), error))?;
            write_create(
                &record,
                &WriterRecord::initialized(workspace_id, &writer, &handoff),
            )?;
        }
        Self::new(state_root, workspace_id, uid).load().map(|_| ())
    }

    /// Whether this workspace has a writer record path entry at all.
    ///
    /// Only a true absence of the path entry (`symlink_metadata` reporting
    /// `NotFound`) means "no writer authority". A dangling symlink is an entry, so
    /// it is reported present and the secure loader refuses it: an unobservable
    /// record must never read as "no writer".
    #[must_use]
    pub fn record_exists(&self) -> bool {
        match fs::symlink_metadata(self.record_path()) {
            Ok(_) => true,
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        }
    }

    pub fn load(&self) -> Result<WriterRecord, MachineError> {
        verify_secure_file(&self.record_path(), self.uid)?;
        let bytes =
            fs::read(self.record_path()).map_err(|error| path_error(&self.record_path(), error))?;
        if bytes.len() > 64 * 1024 {
            return Err(invariant("writer authority record exceeds 64 KiB"));
        }
        let wire: Value = serde_json::from_slice(&bytes)
            .map_err(|_| invariant("writer authority record is not valid JSON"))?;
        match wire.get("schema_version").and_then(Value::as_u64) {
            Some(1) if wire.get("generation_release").is_none() => {}
            Some(2) if wire.get("generation_release").is_some_and(Value::is_object) => {}
            _ => return Err(invariant("unsupported private Writer record version")),
        }
        let record: WriterRecord = serde_json::from_slice(&bytes)
            .map_err(|_| invariant("writer authority record is not valid JSON"))?;
        record.validate(&self.workspace_id)?;
        self.verify_recorded_lock(
            &record,
            LOCK_NAME,
            record.writer_lock_device,
            record.writer_lock_inode,
        )?;
        self.verify_recorded_lock(
            &record,
            HANDOFF_LOCK_NAME,
            record.handoff_lock_device,
            record.handoff_lock_inode,
        )?;
        Ok(record)
    }

    pub fn transact<T>(
        &self,
        operation: impl FnOnce(&mut WriterRecord) -> Result<T, MachineError>,
    ) -> Result<(WriterRecord, T), MachineError> {
        let lock = self.open_lock(LOCK_NAME)?;
        crate::darwin::DarwinSystem
            .lock_exclusive(&lock)
            .map_err(|error| path_error(&self.lock_path(LOCK_NAME), error))?;
        self.revalidate_lock(&lock, LOCK_NAME)?;
        let mut record = self.load()?;
        let previous_release = record.generation_release.clone();
        let result = operation(&mut record)?;
        if let Some(previous) = previous_release.as_ref().filter(|r| r.pending())
            && record.generation_release.as_ref().is_none_or(|r| {
                r.transaction_id != previous.transaction_id
                    || r.source != previous.source
                    || r.writer_generation != previous.writer_generation
            })
        {
            return Err(invariant(
                "generic mutation cannot discard the original release owner",
            ));
        }
        if let Some(previous) = previous_release.as_ref()
            && record.generation_release.as_ref() != Some(previous)
            && !previous.pending()
        {
            retain_release_evidence(
                &self.state_root,
                previous.source.run_id,
                &serde_json::to_value(previous)
                    .map_err(|_| invariant("release archive encoding failed"))?,
            )?;
        }
        record.authority_revision = record
            .authority_revision
            .checked_add(1)
            .ok_or_else(|| invariant("writer authority revision overflow"))?;
        record.validate(&self.workspace_id)?;
        self.replace(&record)?;
        crate::darwin::DarwinSystem
            .unlock(&lock)
            .map_err(|error| path_error(&self.lock_path(LOCK_NAME), error))?;
        Ok((record, result))
    }

    /// Capture authority while a short local observer operation completes.
    /// Callers acquire this before a Run ledger mutex. The sole control-wait
    /// exception is explicit quiescent build handoff: it already holds the Run
    /// lifecycle and startup locks, proves no pending effect or writer holder,
    /// then uses bounded idle Status/Shutdown and verified exit checks. Those
    /// control paths acquire no writer/startup/home/server locks; this boundary
    /// is released before replacement startup. Other callers must not perform
    /// network, process, or worker-control operations in the closure.
    pub fn observe_locked<T>(
        &self,
        observe: impl FnOnce(&WriterRecord) -> T,
    ) -> Result<T, MachineError> {
        let lock = self.open_lock(LOCK_NAME)?;
        crate::darwin::DarwinSystem
            .lock_exclusive(&lock)
            .map_err(|error| path_error(&self.lock_path(LOCK_NAME), error))?;
        self.revalidate_lock(&lock, LOCK_NAME)?;
        let record = self.load()?;
        let result = observe(&record);
        crate::darwin::DarwinSystem
            .unlock(&lock)
            .map_err(|error| path_error(&self.lock_path(LOCK_NAME), error))?;
        Ok(result)
    }

    pub fn transact_handoff<T>(
        &self,
        operation: impl FnOnce(&mut WriterRecord) -> Result<T, MachineError>,
    ) -> Result<(WriterRecord, T), MachineError> {
        let lock = self.open_lock(HANDOFF_LOCK_NAME)?;
        crate::darwin::DarwinSystem
            .lock_exclusive(&lock)
            .map_err(|error| path_error(&self.lock_path(HANDOFF_LOCK_NAME), error))?;
        self.revalidate_lock(&lock, HANDOFF_LOCK_NAME)?;
        let result = self.transact(operation);
        crate::darwin::DarwinSystem
            .unlock(&lock)
            .map_err(|error| path_error(&self.lock_path(HANDOFF_LOCK_NAME), error))?;
        result
    }

    /// Allocate one workspace-global physical app-server epoch while holding
    /// the same permanent serializer every cross-profile handoff uses. The
    /// counter is separate from writer authority, so starting a read-only
    /// dedicated lane cannot fabricate a writer revision.
    pub fn allocate_server_epoch(&self, floor: u64) -> Result<u64, MachineError> {
        let lock = self.open_lock(HANDOFF_LOCK_NAME)?;
        crate::darwin::DarwinSystem
            .lock_exclusive(&lock)
            .map_err(|error| path_error(&self.lock_path(HANDOFF_LOCK_NAME), error))?;
        self.revalidate_lock(&lock, HANDOFF_LOCK_NAME)?;
        let result = (|| {
            let path = self.state_root.join("runtime").join(SERVER_EPOCH_NAME);
            let current = match fs::read_to_string(&path) {
                Ok(value) => value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| invariant("server epoch counter is invalid"))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => return Err(path_error(&path, error)),
            };
            let next = current
                .max(floor)
                .checked_add(1)
                .ok_or_else(|| invariant("server epoch counter overflow"))?;
            let temporary = path.with_extension(format!("{}.tmp", Uuid::now_v7()));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&temporary)
                .map_err(|error| path_error(&temporary, error))?;
            writeln!(file, "{next}").map_err(|error| path_error(&temporary, error))?;
            file.sync_all()
                .map_err(|error| path_error(&temporary, error))?;
            fs::rename(&temporary, &path).map_err(|error| path_error(&path, error))?;
            crate::workspace::sync_directory(
                path.parent().expect("server epoch counter has parent"),
            )
            .map_err(|error| path_error(&path, error))?;
            Ok(next)
        })();
        crate::darwin::DarwinSystem
            .unlock(&lock)
            .map_err(|error| path_error(&self.lock_path(HANDOFF_LOCK_NAME), error))?;
        result
    }

    /// Refresh only observations under the writer lock; authority generations
    /// and revisions are unchanged by an observer projection update.
    pub fn refresh_observation(&self) -> Result<(), MachineError> {
        let lock = self.open_lock(LOCK_NAME)?;
        match crate::darwin::DarwinSystem.lock_exclusive_nonblocking(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(path_error(&self.lock_path(LOCK_NAME), error)),
        }
        self.revalidate_lock(&lock, LOCK_NAME)?;
        let result = (|| {
            let mut record = self.load()?;
            if record
                .generation_release
                .as_ref()
                .is_some_and(GenerationRelease::pending)
            {
                return Ok(());
            }
            let Some(holder) = record.holder.as_mut() else {
                return Ok(());
            };
            let projection = crate::run::RunStore::new(
                crate::workspace::SystemWorkspacePlatform,
                &self.state_root,
            )
            .load_state_projection(holder.run_id)?;
            if projection.run_generation != holder.run_generation {
                return Ok(());
            }
            let before = holder.clone();
            holder.thread_id = projection.thread_id;
            holder.lifecycle = projection.lifecycle;
            holder.active_turn_id = projection.active_turn_id;
            holder.pending_interaction_count = projection.pending_requests.len();
            holder.last_event_cursor = projection.last_event_cursor;
            if *holder != before {
                self.replace(&record)?;
            }
            Ok(())
        })();
        crate::darwin::DarwinSystem
            .unlock(&lock)
            .map_err(|error| path_error(&self.lock_path(LOCK_NAME), error))?;
        result
    }

    pub fn status_value(&self) -> Result<Value, MachineError> {
        self.refresh_observation()?;
        let record = self.load()?;
        let holder = record.holder.as_ref();
        let run = holder
            .map(|holder| crate::semantic::writer_run_status(&self.state_root, holder.run_id))
            .transpose()?
            .unwrap_or(Value::Null);
        Ok(json!({
            "workspace_id": record.workspace_id,
            "authority_state": record.state.as_str(),
            "writer_run_id": holder.map(|value| value.run_id),
            "controller_id": holder.map(|value| value.controller_id),
            "controller_generation": holder.map(|value| value.controller_generation),
            "writer_generation": record.writer_generation,
            "worker_generation": holder.map(|value| value.worker_generation),
            "profile_server_key": holder.map(|value| value.profile_server_key.clone()),
            "profile_server_epoch": holder.map(|value| value.profile_server_epoch),
            "thread_id": run.get("thread_id").cloned().unwrap_or(Value::Null),
            "active_turn_id": run.get("active_turn_id").cloned().unwrap_or(Value::Null),
            "lifecycle_state": run.get("state").cloned().unwrap_or(Value::Null),
            "pending_interaction_count": run.get("pending_count").cloned().unwrap_or(json!(0)),
            "last_event_cursor": run.get("event_cursor").cloned().unwrap_or(json!("0")),
            "recovery_state": if holder.is_none() { "ready" }
                else if run["state"] == "outcome_unknown" { "outcome_unknown" }
                else if record.state == WriterAuthorityState::BlockedUnknown { "reconciliation_required" }
                else if run["identity_verdict"] == "Match" { "ready" } else { "unverifiable" },
            "execution_lane": "dedicated",
            "dedicated_lane": run.get("server_lane").cloned().unwrap_or(Value::Null),
            "workload_background_state": run.get("workload_background_state").cloned().unwrap_or_else(|| json!({
                "state": "unverified",
                "mechanism": "dedicated_lane_process_census",
                "census_revision": record.authority_revision,
                "observed_process_count": 0,
                "quiescent_since": Value::Null,
                "consecutive_empty_samples": 0
            })),
            "handoff": record.handoff.as_ref().map(|handoff| json!({
                "handoff_id": handoff.handoff_id,
                "status": "prepared",
                "source_run_id": handoff.source_run_id,
                "destination_run_id": handoff.destination_run_id,
                "expected_writer_generation": handoff.expected_writer_generation,
                "expires_at": timestamp_from_unix(handoff.expires_at_unix_seconds),
                "blockers": []
            }))
        }))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the durable holder snapshot binds every independently verified identity"
    )]
    pub fn holder(
        run_id: Uuid,
        profile: String,
        controller: &ControllerBinding,
        run_generation: u64,
        profile_server_key: String,
        profile_server_epoch: u64,
        thread_id: Option<String>,
        lifecycle: RunLifecycle,
    ) -> WriterHolder {
        WriterHolder {
            run_id,
            profile,
            controller_id: controller.identity.controller_id,
            controller_generation: controller.identity.generation,
            run_generation,
            worker_generation: run_generation,
            profile_server_key,
            profile_server_epoch,
            thread_id,
            lifecycle,
            active_turn_id: None,
            pending_interaction_count: 0,
            last_event_cursor: None,
        }
    }

    fn record_path(&self) -> PathBuf {
        self.state_root.join("runtime").join(RECORD_NAME)
    }

    fn lock_path(&self, name: &str) -> PathBuf {
        self.state_root.join("runtime").join("locks").join(name)
    }

    fn open_lock(&self, name: &str) -> Result<File, MachineError> {
        let path = self.lock_path(name);
        verify_secure_file(&path, self.uid)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| path_error(&path, error))
    }

    fn revalidate_lock(&self, file: &File, name: &str) -> Result<(), MachineError> {
        let path = self.lock_path(name);
        let descriptor = file.metadata().map_err(|error| path_error(&path, error))?;
        let pathname = fs::symlink_metadata(&path).map_err(|error| path_error(&path, error))?;
        if descriptor.dev() != pathname.dev() || descriptor.ino() != pathname.ino() {
            return Err(MachineError::runtime_path_collision(
                &path,
                crate::workspace::FileIdentity {
                    device: descriptor.dev(),
                    inode: descriptor.ino(),
                },
                crate::workspace::FileIdentity {
                    device: pathname.dev(),
                    inode: pathname.ino(),
                },
            ));
        }
        Ok(())
    }

    fn verify_recorded_lock(
        &self,
        _record: &WriterRecord,
        name: &str,
        device: u64,
        inode: u64,
    ) -> Result<(), MachineError> {
        let path = self.lock_path(name);
        let metadata = fs::symlink_metadata(&path).map_err(|error| path_error(&path, error))?;
        if metadata.dev() != device || metadata.ino() != inode {
            return Err(MachineError::runtime_path_collision(
                &path,
                crate::workspace::FileIdentity { device, inode },
                crate::workspace::FileIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                },
            ));
        }
        Ok(())
    }

    fn replace(&self, record: &WriterRecord) -> Result<(), MachineError> {
        let path = self.record_path();
        let parent = path.parent().expect("writer record has parent");
        let temporary = parent.join(format!(".writer-{}.tmp", Uuid::now_v7()));
        let mut bytes =
            serde_json::to_vec(record).map_err(|_| invariant("writer record encoding failed"))?;
        bytes.push(b'\n');
        if bytes.len() > 64 * 1024 {
            return Err(invariant("writer authority record exceeds 64 KiB"));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|error| path_error(&temporary, error))?;
        file.write_all(&bytes)
            .map_err(|error| path_error(&temporary, error))?;
        file.sync_all()
            .map_err(|error| path_error(&temporary, error))?;
        fs::rename(&temporary, &path).map_err(|error| path_error(&path, error))?;
        sync_directory(parent)
    }
}

/// Content-addressed, immutable evidence owned by the Run's Writer release.
/// Publication precedes the Writer checkpoint which references its digest.
pub(crate) fn retain_release_evidence(
    state_root: &Path,
    run_id: Uuid,
    value: &Value,
) -> Result<String, MachineError> {
    let run_root = state_root.join("runs").join(run_id.to_string());
    let root = run_root.join("release-evidence");
    let uid = crate::darwin::DarwinSystem.current_uid();
    verify_secure_directory(&run_root, uid)?;
    match crate::workspace::create_directory(&root, 0o700) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(path_error(&root, error)),
    }
    verify_secure_directory(&root, uid)?;
    let bytes =
        serde_json::to_vec(value).map_err(|_| invariant("release evidence encoding failed"))?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(invariant("release evidence exceeds bound"));
    }
    let digest = crate::jcs::sha256_hex(&bytes);
    let path = root.join(format!("generation-release-{digest}.json"));
    publish_immutable_release_file(&root, &path, &bytes)?;
    Ok(digest)
}

/// Fsync a private staging inode before atomically publishing the immutable
/// name. A crash while writing can leave only an unreferenced staging file.
pub(crate) fn publish_immutable_release_file(
    root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<(), MachineError> {
    let uid = crate::darwin::DarwinSystem.current_uid();
    verify_secure_directory(root, uid)?;
    let temporary = root.join(format!(".generation-release-{}.tmp", Uuid::now_v7()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|e| path_error(&temporary, e))?;
    let result = (|| {
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| path_error(&temporary, e))?;
        match crate::darwin::DarwinSystem.rename_exclusive(&temporary, path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_secure_file(path, uid)?;
                if fs::read(path).map_err(|e| path_error(path, e))? != bytes {
                    return Err(invariant("immutable release evidence collision"));
                }
            }
            Err(e) => return Err(path_error(path, e)),
        }
        Ok(())
    })();
    // Only this invocation's staging inode is removed. The final name is
    // immutable and is never replaced or repaired in place.
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(path_error(&temporary, e)),
    }
    sync_directory(root)?;
    result
}

pub(crate) fn read_release_evidence(
    state_root: &Path,
    run_id: Uuid,
    digest: &str,
) -> Result<Value, MachineError> {
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invariant("invalid release evidence digest"));
    }
    let run_root = state_root.join("runs").join(run_id.to_string());
    let root = run_root.join("release-evidence");
    let uid = crate::darwin::DarwinSystem.current_uid();
    verify_secure_directory(&run_root, uid)?;
    verify_secure_directory(&root, uid)?;
    let path = root.join(format!("generation-release-{digest}.json"));
    verify_secure_file(&path, uid)?;
    if fs::symlink_metadata(&path)
        .map_err(|e| path_error(&path, e))?
        .len()
        > 4 * 1024 * 1024
    {
        return Err(invariant("release evidence exceeds bound"));
    }
    let bytes = fs::read(&path).map_err(|e| path_error(&path, e))?;
    if crate::jcs::sha256_hex(&bytes) != digest {
        return Err(invariant("release evidence digest changed"));
    }
    serde_json::from_slice(&bytes).map_err(|_| invariant("release evidence is invalid"))
}

pub fn writer_busy(record: &WriterRecord, requested_run_id: Uuid) -> MachineError {
    MachineError::new(
        "WRITER_BUSY",
        "another run owns durable workspace writer authority",
        true,
        json!({
            "requested_run_id": requested_run_id,
            "holder_run_id": record.holder.as_ref().map(|holder| holder.run_id),
            "holder_profile": record.holder.as_ref().map(|holder| holder.profile.clone()),
            "authority_state": record.state.as_str(),
            "controller_kind": Value::Null,
            "writer_generation": record.writer_generation,
            "handoff_eligible": record.state == WriterAuthorityState::Active,
        }),
    )
}

pub fn timestamp_from_unix(seconds: u64) -> String {
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(i64::MAX));
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3_600,
        day_seconds % 3_600 / 60,
        day_seconds % 60,
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_epoch.saturating_add(719_468);
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn create_permanent_lock(path: &Path, uid: u32) -> Result<(), MachineError> {
    if !path.exists() {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| path_error(path, error))?;
        file.sync_all().map_err(|error| path_error(path, error))?;
        sync_directory(path.parent().expect("lock has parent"))?;
    }
    verify_secure_file(path, uid)
}

fn write_create(path: &Path, record: &WriterRecord) -> Result<(), MachineError> {
    let mut bytes =
        serde_json::to_vec(record).map_err(|_| invariant("writer record encoding failed"))?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| path_error(path, error))?;
    file.write_all(&bytes)
        .map_err(|error| path_error(path, error))?;
    file.sync_all().map_err(|error| path_error(path, error))?;
    sync_directory(path.parent().expect("record has parent"))
}

fn sync_directory(path: &Path) -> Result<(), MachineError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| path_error(path, error))
}

fn path_error(path: &Path, error: std::io::Error) -> MachineError {
    MachineError::runtime_path_invalid(path, error.to_string())
}

fn invariant(reason: &str) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "writer authority requires reconciliation",
        false,
        json!({"reason": reason, "required_action": "operator_repair"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ControllerIdentity, ControllerKind};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("dolgorae-writer-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("runtime")).unwrap();
        fs::set_permissions(root.join("runtime"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("runtime/locks")).unwrap();
        fs::set_permissions(
            root.join("runtime/locks"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        root
    }

    fn holder(run_id: Uuid) -> WriterHolder {
        WriterStore::holder(
            run_id,
            "profile".to_owned(),
            &ControllerBinding {
                identity: ControllerIdentity {
                    controller_id: Uuid::now_v7(),
                    kind: ControllerKind::HumanCli,
                    instance_id: "master".to_owned(),
                    subject_id: None,
                    generation: 1,
                },
                capability_sha256: "a".repeat(64),
            },
            1,
            "b".repeat(64),
            1,
            Some("thread".to_owned()),
            RunLifecycle::Idle,
        )
    }

    fn generation_release(record: &WriterRecord, operation: Uuid) -> GenerationRelease {
        GenerationRelease {
            purpose: GenerationRetirementPurpose::Release,
            transaction_id: operation,
            source: record.holder.clone().unwrap(),
            writer_generation: record.writer_generation,
            authority_revision: record.authority_revision,
            source_revision: 1,
            source_ledger_sha256: "1".repeat(64),
            source_runtime_sha256: "2".repeat(64),
            registration_sha256: "3".repeat(64),
            scope_sha256: None,
            absence_sha256: None,
            reader_proof_sha256: None,
            retired_reader_sha256: None,
            phase: GenerationReleasePhase::Prepared,
            successor: None,
        }
    }

    #[test]
    fn startup_abort_releases_only_the_initial_threadless_reservation() {
        let root = root();
        let workspace = "a".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let run = Uuid::now_v7();
        let foreign = Uuid::now_v7();
        let mut initial = holder(run);
        initial.thread_id = None;
        initial.lifecycle = RunLifecycle::Starting;
        let (blocked, ()) = store
            .transact(|record| {
                record.prepare_acquire(run, initial, run)?;
                record.block_unknown(run);
                record.release_aborted_initial_acquire(foreign)
            })
            .unwrap();
        assert_eq!(blocked.state, WriterAuthorityState::BlockedUnknown);
        assert!(
            store
                .transact(|record| {
                    record.holder.as_mut().unwrap().thread_id = Some("other-thread".into());
                    record.release_aborted_initial_acquire(run)
                })
                .is_err()
        );
        assert_eq!(store.load().unwrap(), blocked);
        let (released, ()) = store
            .transact(|record| record.release_aborted_initial_acquire(run))
            .unwrap();
        assert_eq!(released.state, WriterAuthorityState::None);
        assert_eq!(released.writer_generation, blocked.writer_generation);
        store
            .transact(|record| {
                record.prepare_acquire(foreign, holder(foreign), foreign)?;
                record.release_aborted_initial_acquire(run)
            })
            .unwrap();
        assert_eq!(store.load().unwrap().holder.unwrap().run_id, foreign);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_scope_survives_restart_without_approving_release() {
        let root = root();
        let workspace = "a".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let run = Uuid::now_v7();
        let operation = Uuid::now_v7();
        store
            .transact(|record| {
                let token = Uuid::now_v7();
                let (_, generation) = record.prepare_acquire(run, holder(run), token)?;
                record.commit_acquire(token, generation)?;
                record.prepare_release(run, operation)?;
                record.prepare_generation_release(generation_release(record, operation))
            })
            .unwrap();
        let original = store.load().unwrap().generation_release.unwrap();
        store
            .transact(|r| r.retain_prepared_scope(&original, "4".repeat(64)))
            .unwrap();
        let restarted = WriterStore::new(&root, &workspace, uid);
        let record = restarted.load().unwrap();
        let retained = record.generation_release.clone().unwrap();
        assert_eq!(retained.phase, GenerationReleasePhase::Prepared);
        assert_eq!(retained.transaction_id, operation);
        assert_eq!(retained.scope_sha256, Some("4".repeat(64)));
        assert_eq!(record.state, WriterAuthorityState::Releasing);
        let bytes = fs::read(store.record_path()).unwrap();
        assert!(
            store
                .transact(|r| r.retain_prepared_scope(&original, "5".repeat(64)))
                .is_err()
        );
        assert!(
            store
                .transact(|r| r.retain_prepared_scope(&retained, "5".repeat(64)))
                .is_err()
        );
        assert!(store.transact(|r| r.commit_release(operation)).is_err());
        assert_eq!(fs::read(store.record_path()).unwrap(), bytes);
        let mut next = retained.clone();
        next.phase = GenerationReleasePhase::StopStarted;
        store
            .transact(|r| r.advance_generation_release(&retained, next))
            .unwrap();
        assert_eq!(store.load().unwrap().transaction_id, Some(operation));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generation_release_keeps_one_durable_owner_through_crash_phases_and_commit_replay() {
        let root = root();
        let workspace = "a".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let original = fs::read(store.record_path()).unwrap();
        assert_eq!(store.load().unwrap().schema_version, 1);
        assert_eq!(fs::read(store.record_path()).unwrap(), original);
        let run = Uuid::now_v7();
        let operation = Uuid::now_v7();
        let recovery = root.join("runs").join(run.to_string()).join("recovery");
        for directory in [
            root.join("runs"),
            root.join("runs").join(run.to_string()),
            recovery.clone(),
        ] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
        }
        store
            .transact(|record| {
                let (_, generation) = record.prepare_acquire(run, holder(run), Uuid::now_v7())?;
                let token = record.transaction_id.unwrap();
                record.commit_acquire(token, generation)?;
                record.prepare_release(run, operation)?;
                record.prepare_generation_release(generation_release(record, operation))
            })
            .unwrap();
        let mut checkpoint = store.load().unwrap().generation_release.unwrap();
        let mut wrong = checkpoint.clone();
        wrong.phase = GenerationReleasePhase::OldAbsent;
        wrong.scope_sha256 = Some("4".repeat(64));
        wrong.absence_sha256 = Some("5".repeat(64));
        assert!(
            store
                .transact(|r| r.advance_generation_release(&checkpoint, wrong))
                .is_err()
        );
        for phase in [
            GenerationReleasePhase::StopStarted,
            GenerationReleasePhase::OldAbsent,
            GenerationReleasePhase::ReaderStarting,
            GenerationReleasePhase::ReaderVerified,
        ] {
            let mut next = checkpoint.clone();
            next.phase = phase;
            if phase == GenerationReleasePhase::StopStarted {
                next.scope_sha256 = Some("4".repeat(64));
            }
            if phase == GenerationReleasePhase::OldAbsent {
                next.absence_sha256 = Some("5".repeat(64));
            }
            if phase == GenerationReleasePhase::ReaderStarting {
                next.successor = Some(ReaderReservation {
                    run_generation: 2,
                    server_epoch: 2,
                    control_socket_epoch: 2,
                    attempt_started: true,
                });
            }
            if phase == GenerationReleasePhase::ReaderVerified {
                next.reader_proof_sha256 = Some("6".repeat(64));
            }
            store
                .transact(|r| r.advance_generation_release(&checkpoint, next))
                .unwrap();
            let recovered = store.load().unwrap();
            checkpoint = recovered.generation_release.clone().unwrap();
            assert_eq!(recovered.transaction_id, Some(operation));
            assert_eq!(recovered.holder.as_ref(), Some(&checkpoint.source));
            assert!(
                store
                    .transact(|r| r.retire_verified_reader_guard(operation))
                    .is_err()
            );
            assert!(store.transact(|r| r.commit_release(operation)).is_err());
            store
                .transact(|r| {
                    r.rollback_release(operation);
                    r.block_unknown(operation);
                    r.rollback_release(operation);
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                store.load().unwrap().state,
                WriterAuthorityState::BlockedUnknown
            );
            assert!(
                store
                    .load()
                    .unwrap()
                    .require_no_generation_release(run)
                    .is_err()
            );
        }
        let mut committed = checkpoint.clone();
        committed.phase = GenerationReleasePhase::Committed;
        store
            .transact(|r| {
                r.advance_generation_release(&checkpoint, committed)?;
                r.commit_release(operation)
            })
            .unwrap();
        let final_owner = store.load().unwrap().generation_release.clone();
        assert_eq!(store.load().unwrap().state, WriterAuthorityState::None);
        let next_run = Uuid::now_v7();
        store
            .transact(|r| {
                r.prepare_acquire(next_run, holder(next_run), Uuid::now_v7())?;
                Ok(())
            })
            .unwrap();
        assert_eq!(store.load().unwrap().generation_release, final_owner);
        assert!(store.transact(|r| r.commit_release(operation)).is_err());
        let previous = final_owner.unwrap();
        let archive = serde_json::to_value(&previous).unwrap();
        let next_operation = Uuid::now_v7();
        store
            .transact(|record| {
                let token = record.transaction_id.unwrap();
                record.commit_acquire(token, record.writer_generation)?;
                record.prepare_release(next_run, next_operation)?;
                record.prepare_generation_release(generation_release(record, next_operation))
            })
            .unwrap();
        let digest = crate::jcs::sha256_hex(&serde_json::to_vec(&archive).unwrap());
        assert_eq!(read_release_evidence(&root, run, &digest).unwrap(), archive);
        let archive_path = root
            .join("runs")
            .join(run.to_string())
            .join("release-evidence")
            .join(format!("generation-release-{digest}.json"));
        let archive_inode = fs::metadata(&archive_path).unwrap().ino();
        assert_eq!(
            retain_release_evidence(&root, run, &archive).unwrap(),
            digest
        );
        assert_eq!(fs::metadata(&archive_path).unwrap().ino(), archive_inode);
        assert_eq!(fs::read_dir(&recovery).unwrap().count(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn acquire_retirement_preserves_reservation_and_recovery_never_commits_write() {
        for continue_acquire in [false, true] {
            let root = root();
            let workspace = "a".repeat(64);
            let uid = crate::darwin::DarwinSystem.current_uid();
            WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
            let store = WriterStore::new(&root, &workspace, uid);
            let run = Uuid::now_v7();
            for directory in [root.join("runs"), root.join("runs").join(run.to_string())] {
                fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
            }
            let operation = Uuid::now_v7();
            store
                .transact(|record| {
                    record.prepare_acquire(run, holder(run), operation)?;
                    let mut owner = generation_release(record, operation);
                    owner.purpose = GenerationRetirementPurpose::Acquire;
                    record.prepare_generation_release(owner)
                })
                .unwrap();
            for phase in [
                GenerationReleasePhase::StopStarted,
                GenerationReleasePhase::OldAbsent,
                GenerationReleasePhase::ReaderStarting,
                GenerationReleasePhase::ReaderVerified,
            ] {
                let owner = store.load().unwrap().generation_release.unwrap();
                store
                    .transact(|record| {
                        let mut next = owner.clone();
                        next.phase = phase;
                        if phase == GenerationReleasePhase::StopStarted {
                            next.scope_sha256 = Some("4".repeat(64));
                        }
                        if phase == GenerationReleasePhase::OldAbsent {
                            next.absence_sha256 = Some("5".repeat(64));
                        }
                        if phase == GenerationReleasePhase::ReaderStarting {
                            next.successor = Some(ReaderReservation {
                                run_generation: 2,
                                server_epoch: 2,
                                control_socket_epoch: 2,
                                attempt_started: true,
                            });
                        }
                        if phase == GenerationReleasePhase::ReaderVerified {
                            next.reader_proof_sha256 = Some("6".repeat(64));
                        }
                        let mut wrong = next.clone();
                        wrong.purpose = GenerationRetirementPurpose::Release;
                        assert!(record.advance_generation_release(&owner, wrong).is_err());
                        record.advance_generation_release(&owner, next)
                    })
                    .unwrap();
                store
                    .transact(|record| {
                        record.block_unknown(operation);
                        Ok(())
                    })
                    .unwrap();
                let current = store.load().unwrap();
                assert_eq!(current.transaction_id, Some(operation));
                assert_eq!(current.holder.as_ref(), Some(&owner.source));
                assert!(current.require_no_generation_release(run).is_err());
            }
            let owner = store.load().unwrap().generation_release.unwrap();
            store
                .transact(|record| record.commit_generation_retirement(&owner, continue_acquire))
                .unwrap();
            let current = store.load().unwrap();
            assert_eq!(
                current.generation_release.as_ref().unwrap().phase,
                GenerationReleasePhase::Committed
            );
            if continue_acquire {
                assert_eq!(current.state, WriterAuthorityState::Reserved);
                assert_eq!(current.transaction_id, Some(operation));
                assert_eq!(current.writer_generation, owner.writer_generation);
                assert_eq!(current.holder.as_ref().unwrap().run_generation, 2);
                assert_eq!(
                    current.holder.as_ref().unwrap().thread_id,
                    owner.source.thread_id
                );
                assert!(
                    store
                        .transact(
                            |record| record.commit_acquire(Uuid::now_v7(), owner.writer_generation)
                        )
                        .is_err()
                );
                let original = fs::read(store.record_path()).unwrap();
                for state in [
                    WriterAuthorityState::Reserved,
                    WriterAuthorityState::BlockedUnknown,
                ] {
                    for field in ["holder", "transaction", "reader"] {
                        let mut invalid = current.clone();
                        invalid.state = state;
                        match field {
                            "holder" => invalid.holder.as_mut().unwrap().run_generation += 1,
                            "transaction" => invalid.transaction_id = Some(Uuid::now_v7()),
                            "reader" => {
                                invalid
                                    .generation_release
                                    .as_mut()
                                    .unwrap()
                                    .successor
                                    .as_mut()
                                    .unwrap()
                                    .server_epoch += 1
                            }
                            _ => unreachable!(),
                        }
                        fs::write(store.record_path(), serde_json::to_vec(&invalid).unwrap())
                            .unwrap();
                        assert!(store.load().is_err(), "{state:?}/{field}");
                    }
                }
                fs::write(store.record_path(), &original).unwrap();
                let mut successful = current.clone();
                successful
                    .commit_acquire(operation, owner.writer_generation)
                    .unwrap();
                assert_eq!(successful.state, WriterAuthorityState::Active);
                assert!(successful.generation_release.is_none());
                assert!(successful.acquire_retirement_recovery(run).is_none());
                store
                    .transact(|record| {
                        record.block_unknown(operation);
                        Ok(())
                    })
                    .unwrap();
                assert!(
                    store
                        .transact(|record| record
                            .commit_verified_recovered_acquire(operation, owner.writer_generation))
                        .is_err()
                );
                store
                    .transact(|record| {
                        record.prepare_acquire_retirement_recovery(run)?;
                        let mut latest = generation_release(record, operation);
                        latest.purpose = GenerationRetirementPurpose::Acquire;
                        assert_eq!(latest.source.run_generation, 2);
                        assert!(latest.scope_sha256.is_none() && latest.absence_sha256.is_none());
                        assert!(record.commit_generation_retirement(&latest, false).is_err());
                        record.prepare_generation_release(latest)
                    })
                    .unwrap();
                let pending = store.load().unwrap();
                assert_eq!(pending.transaction_id, Some(operation));
                assert_eq!(
                    pending
                        .generation_release
                        .as_ref()
                        .unwrap()
                        .source
                        .run_generation,
                    2
                );
                assert_eq!(pending.state, WriterAuthorityState::Reserved);
            } else {
                assert_eq!(current.state, WriterAuthorityState::None);
                assert!(current.holder.is_none());
                assert!(current.transaction_id.is_none());
                let next_run = Uuid::now_v7();
                let next_operation = Uuid::now_v7();
                store
                    .transact(|record| {
                        let mut next_holder = holder(next_run);
                        next_holder.thread_id = None;
                        let (_, next_generation) =
                            record.prepare_acquire(next_run, next_holder, next_operation)?;
                        assert!(record.generation_release.is_none());
                        record.commit_acquire(next_operation, next_generation)
                    })
                    .unwrap();
                let next = store.load().unwrap();
                assert_eq!(next.state, WriterAuthorityState::Active);
                assert_eq!(next.holder.as_ref().unwrap().run_id, next_run);
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn generation_release_evidence_refuses_unsafe_namespace_without_repair() {
        use std::os::unix::fs::symlink;

        let state_root = root();
        let run_id = Uuid::now_v7();
        let run_root = state_root.join("runs").join(run_id.to_string());
        for path in [state_root.join("runs"), run_root.clone()] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        let evidence = json!({"run_id":run_id});
        let digest = crate::jcs::sha256_hex(&serde_json::to_vec(&evidence).unwrap());
        let namespace = run_root.join("release-evidence");
        assert!(read_release_evidence(&state_root, run_id, &digest).is_err());
        assert!(!namespace.exists());
        let outside = state_root.join("outside");
        fs::DirBuilder::new().mode(0o700).create(&outside).unwrap();
        symlink(&outside, &namespace).unwrap();
        assert!(retain_release_evidence(&state_root, run_id, &evidence).is_err());
        assert!(read_release_evidence(&state_root, run_id, &digest).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        fs::remove_file(&namespace).unwrap();
        fs::DirBuilder::new()
            .mode(0o755)
            .create(&namespace)
            .unwrap();
        fs::set_permissions(&namespace, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(retain_release_evidence(&state_root, run_id, &evidence).is_err());
        assert_eq!(fs::metadata(&namespace).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::read_dir(&namespace).unwrap().count(), 0);
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn generation_release_reader_retry_requires_absence_and_never_reuses_reservation() {
        let run = Uuid::now_v7();
        let operation = Uuid::now_v7();
        let mut record = WriterRecord::empty(&"a".repeat(64));
        let (_, generation) = record
            .prepare_acquire(run, holder(run), Uuid::now_v7())
            .unwrap();
        let token = record.transaction_id.unwrap();
        record.commit_acquire(token, generation).unwrap();
        record.prepare_release(run, operation).unwrap();
        let mut owner = generation_release(&record, operation);
        owner.phase = GenerationReleasePhase::ReaderStarting;
        owner.scope_sha256 = Some("4".repeat(64));
        owner.absence_sha256 = Some("5".repeat(64));
        owner.successor = Some(ReaderReservation {
            run_generation: 2,
            server_epoch: 2,
            control_socket_epoch: 2,
            attempt_started: true,
        });
        record.schema_version = 2;
        record.generation_release = Some(owner.clone());
        let mut verified = owner.clone();
        verified.phase = GenerationReleasePhase::ReaderVerified;
        verified.reader_proof_sha256 = Some("8".repeat(64));
        let mut verified_record = record.clone();
        verified_record.generation_release = Some(verified.clone());
        verified_record
            .replace_absent_reader(
                &verified,
                ReaderReservation {
                    run_generation: 3,
                    server_epoch: 3,
                    control_socket_epoch: 3,
                    attempt_started: false,
                },
                "9".repeat(64),
            )
            .unwrap();
        let resumed = verified_record.generation_release.unwrap();
        assert_eq!(resumed.phase, GenerationReleasePhase::ReaderStarting);
        assert!(resumed.reader_proof_sha256.is_none());
        assert_eq!(resumed.transaction_id, owner.transaction_id);
        assert_eq!(resumed.source, owner.source);
        let old = owner.successor.clone().unwrap();
        for bad in [
            ReaderReservation {
                attempt_started: false,
                ..old.clone()
            },
            ReaderReservation {
                run_generation: 3,
                attempt_started: false,
                ..old.clone()
            },
            ReaderReservation {
                run_generation: 3,
                server_epoch: 3,
                attempt_started: false,
                ..old.clone()
            },
        ] {
            assert!(
                record
                    .replace_absent_reader(&owner, bad, "6".repeat(64))
                    .is_err()
            );
        }
        let replacement = ReaderReservation {
            run_generation: 3,
            server_epoch: 3,
            control_socket_epoch: 3,
            attempt_started: false,
        };
        assert!(
            record
                .replace_absent_reader(&owner, replacement.clone(), "bad".to_owned())
                .is_err()
        );
        record
            .replace_absent_reader(&owner, replacement.clone(), "6".repeat(64))
            .unwrap();
        assert_eq!(
            record
                .generation_release
                .as_ref()
                .unwrap()
                .successor
                .as_ref(),
            Some(&replacement)
        );
        assert!(
            record
                .replace_absent_reader(&owner, replacement, "7".repeat(64))
                .is_err()
        );
        assert_eq!(record.transaction_id, Some(operation));
        assert_eq!(record.holder.as_ref(), Some(&owner.source));
    }

    #[test]
    fn generation_release_versions_and_immutable_publication_fail_closed() {
        let root = root();
        let workspace = "a".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let old = fs::read(store.record_path()).unwrap();
        let mut wire: Value = serde_json::from_slice(&old).unwrap();
        for version in [0, 2, 3] {
            wire["schema_version"] = json!(version);
            fs::write(store.record_path(), serde_json::to_vec(&wire).unwrap()).unwrap();
            assert!(store.load().is_err());
        }
        wire["schema_version"] = json!(1);
        wire["generation_release"] = Value::Null;
        fs::write(store.record_path(), serde_json::to_vec(&wire).unwrap()).unwrap();
        assert!(store.load().is_err());
        fs::write(store.record_path(), &old).unwrap();
        let run = Uuid::now_v7();
        let operation = Uuid::now_v7();
        let mut record = store.load().unwrap();
        let (_, generation) = record.prepare_acquire(run, holder(run), operation).unwrap();
        record.commit_acquire(operation, generation).unwrap();
        record.prepare_release(run, operation).unwrap();
        let historical = generation_release(&record, operation);
        let original_bytes = serde_json::to_vec(&historical).unwrap();
        assert!(
            serde_json::to_value(&historical)
                .unwrap()
                .get("purpose")
                .is_none()
        );
        let decoded: GenerationRelease = serde_json::from_slice(&original_bytes).unwrap();
        assert_eq!(decoded.purpose, GenerationRetirementPurpose::Release);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), original_bytes);
        store
            .transact(|current| {
                current.prepare_acquire(run, holder(run), operation)?;
                let mut acquire = generation_release(current, operation);
                acquire.purpose = GenerationRetirementPurpose::Acquire;
                current.prepare_generation_release(acquire)
            })
            .unwrap();
        for blocked in [false, true] {
            if blocked {
                store
                    .transact(|current| {
                        current.block_unknown(operation);
                        Ok(())
                    })
                    .unwrap();
            }
            let current = store.load().unwrap();
            assert_eq!(
                current.state,
                if blocked {
                    WriterAuthorityState::BlockedUnknown
                } else {
                    WriterAuthorityState::Reserved
                }
            );
            let value = serde_json::to_value(&current).unwrap();
            assert_eq!(value["generation_release"]["purpose"], "acquire");
        }
        let path = root.join("immutable.json");
        let pending = root.join(".generation-release-interrupted.tmp");
        fs::write(&pending, b"partial").unwrap();
        publish_immutable_release_file(&root, &path, b"complete").unwrap();
        publish_immutable_release_file(&root, &path, b"complete").unwrap();
        assert!(publish_immutable_release_file(&root, &path, b"different").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"complete");
        assert_eq!(fs::symlink_metadata(&path).unwrap().nlink(), 1);
        assert_eq!(fs::read(&pending).unwrap(), b"partial");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn initializes_and_revises_one_canonical_record() {
        let root = root();
        let workspace = "a".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let initial = store.load().unwrap();
        assert_eq!(initial.state, WriterAuthorityState::None);
        assert_eq!(initial.authority_revision, 0);
        assert_ne!(initial.writer_lock_inode, 0);
        assert_ne!(initial.handoff_lock_inode, 0);
        let (record, ()) = store
            .transact(|record| {
                record.state = WriterAuthorityState::BlockedUnknown;
                record.writer_generation = 1;
                record.transaction_id = Some(Uuid::now_v7());
                record.holder = Some(WriterHolder {
                    run_id: Uuid::now_v7(),
                    profile: "writer".to_owned(),
                    controller_id: Uuid::now_v7(),
                    controller_generation: 1,
                    run_generation: 1,
                    worker_generation: 1,
                    profile_server_key: "b".repeat(64),
                    profile_server_epoch: 1,
                    thread_id: Some("thread".to_owned()),
                    lifecycle: RunLifecycle::Idle,
                    active_turn_id: None,
                    pending_interaction_count: 0,
                    last_event_cursor: None,
                });
                record.recovery_action = Some("operator_repair".to_owned());
                Ok(())
            })
            .unwrap();
        assert_eq!(record.authority_revision, 1);
        assert_eq!(store.load().unwrap(), record);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn caller_guard_recovery_preserves_generation_and_rejects_stale_retirement() {
        let root = root();
        let workspace = "f".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let run_id = Uuid::now_v7();
        let acquire = Uuid::now_v7();
        store
            .transact(|record| {
                let (_, generation) = record.prepare_acquire(run_id, holder(run_id), acquire)?;
                record.commit_acquire(acquire, generation)
            })
            .unwrap();
        let original = store.load().unwrap();
        let pending = Uuid::now_v7();
        store
            .transact(|record| {
                record.prepare_release(run_id, pending)?;
                record.block_unknown(pending);
                Ok(())
            })
            .unwrap();
        let guarded = store.load().unwrap();
        assert!(
            store
                .transact(|record| record
                    .commit_verified_recovered_acquire(Uuid::now_v7(), original.writer_generation,))
                .is_err()
        );
        assert_eq!(store.load().unwrap(), guarded);
        store
            .transact(|record| {
                record.commit_verified_recovered_acquire(pending, original.writer_generation)
            })
            .unwrap();
        let recovered = store.load().unwrap();
        assert_eq!(recovered.state, WriterAuthorityState::Active);
        assert_eq!(recovered.holder, original.holder);
        assert_eq!(recovered.writer_generation, original.writer_generation);
        let retirement = Uuid::now_v7();
        store
            .transact(|record| {
                record.prepare_release(run_id, retirement)?;
                record.block_unknown(retirement);
                record.retire_verified_reader_guard(retirement)
            })
            .unwrap();
        let other_run = Uuid::now_v7();
        let other_transaction = Uuid::now_v7();
        store
            .transact(|record| {
                let (_, generation) =
                    record.prepare_acquire(other_run, holder(other_run), other_transaction)?;
                assert!(generation > original.writer_generation);
                record.commit_acquire(other_transaction, generation)
            })
            .unwrap();
        let current = store.load().unwrap();
        assert!(
            store
                .transact(|record| record.retire_verified_reader_guard(retirement))
                .is_err()
        );
        assert_eq!(store.load().unwrap(), current);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn durable_holder_observations_follow_the_matching_run_generation() {
        let root = root();
        let workspace = "c".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let run_id = Uuid::now_v7();
        let transaction = Uuid::now_v7();
        store
            .transact(|record| {
                let (_, generation) =
                    record.prepare_acquire(run_id, holder(run_id), transaction)?;
                record.commit_acquire(transaction, generation)
            })
            .unwrap();
        let revision = store.load().unwrap().authority_revision;
        let run_root = root.join("runs").join(run_id.to_string());
        fs::create_dir_all(&run_root).unwrap();
        let mut projection = crate::projection::RunStateProjection::starting(run_id);
        projection.run_generation = 1;
        projection.thread_id = Some("bound-thread".to_owned());
        projection.lifecycle = RunLifecycle::OutcomeUnknown;
        projection.pending_requests = vec!["9".to_owned()];
        projection.last_event_cursor = Some("37".to_owned());
        let path = run_root.join("state.json");
        fs::write(&path, serde_json::to_vec(&projection).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        store.refresh_observation().unwrap();
        let observed = store.load().unwrap();
        assert_eq!(observed.authority_revision, revision);
        let observed = observed.holder.unwrap();
        assert_eq!(observed.thread_id.as_deref(), Some("bound-thread"));
        assert_eq!(observed.lifecycle, RunLifecycle::OutcomeUnknown);
        assert_eq!(observed.pending_interaction_count, 1);
        assert_eq!(observed.last_event_cursor.as_deref(), Some("37"));
        projection.run_generation = 2;
        projection.lifecycle = RunLifecycle::Idle;
        fs::write(&path, serde_json::to_vec(&projection).unwrap()).unwrap();
        store.refresh_observation().unwrap();
        assert_eq!(store.load().unwrap().holder.unwrap(), observed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_a_replaced_permanent_lock_inode() {
        let root = root();
        let workspace = "c".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let lock = root.join("runtime/locks/writer.lock");
        let displaced = root.join("runtime/locks/writer.displaced");
        fs::rename(&lock, &displaced).unwrap();
        create_permanent_lock(&lock, uid).unwrap();
        let error = store.load().unwrap_err();
        assert_eq!(error.code, "RUNTIME_PATH_COLLISION");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_a_replaced_handoff_lock_inode() {
        let root = root();
        let workspace = "f".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let lock = root.join("runtime/locks/handoff.lock");
        let displaced = root.join("runtime/locks/handoff.displaced");
        fs::rename(&lock, &displaced).unwrap();
        create_permanent_lock(&lock, uid).unwrap();
        let error = store.load().unwrap_err();
        assert_eq!(error.code, "RUNTIME_PATH_COLLISION");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn distinct_workspace_roots_do_not_share_a_serializer() {
        let first = root();
        let second = root();
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&first, &"d".repeat(64), uid).unwrap();
        WriterStore::initialize_layout(&second, &"e".repeat(64), uid).unwrap();
        let first_store = WriterStore::new(&first, "d".repeat(64), uid);
        let second_store = WriterStore::new(&second, "e".repeat(64), uid);
        first_store
            .transact(|record| {
                record.writer_generation = 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(first_store.load().unwrap().writer_generation, 1);
        assert_eq!(second_store.load().unwrap().writer_generation, 0);
        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn dedicated_server_epochs_are_workspace_global_monotonic_and_durable() {
        let root = root();
        let workspace = "e".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        assert_eq!(store.allocate_server_epoch(40).unwrap(), 41);
        assert_eq!(store.allocate_server_epoch(1).unwrap(), 42);
        assert_eq!(store.allocate_server_epoch(100).unwrap(), 101);
        assert_eq!(store.load().unwrap().authority_revision, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn acquire_and_release_crash_landings_are_closed_and_never_reuse_generation() {
        let root = root();
        let workspace = "1".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let first_run = Uuid::now_v7();
        let first_transaction = Uuid::now_v7();
        let (_, (_, generation)) = store
            .transact(|record| {
                record.prepare_acquire(first_run, holder(first_run), first_transaction)
            })
            .unwrap();
        assert_eq!(generation, 1);
        assert_eq!(store.load().unwrap().state, WriterAuthorityState::Reserved);
        store
            .transact(|record| {
                record.block_unknown(first_transaction);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.load().unwrap().state,
            WriterAuthorityState::BlockedUnknown
        );

        store
            .transact(|record| {
                record.state = WriterAuthorityState::None;
                record.transaction_id = None;
                record.holder = None;
                record.recovery_action = None;
                Ok(())
            })
            .unwrap();
        let second_run = Uuid::now_v7();
        let second_transaction = Uuid::now_v7();
        let (_, (_, next_generation)) = store
            .transact(|record| {
                record.prepare_acquire(second_run, holder(second_run), second_transaction)
            })
            .unwrap();
        assert_eq!(next_generation, 2);
        store
            .transact(|record| record.commit_acquire(second_transaction, next_generation))
            .unwrap();
        let release = Uuid::now_v7();
        store
            .transact(|record| record.prepare_release(second_run, release).map(|_| ()))
            .unwrap();
        assert_eq!(store.load().unwrap().state, WriterAuthorityState::Releasing);
        store
            .transact(|record| {
                record.rollback_release(release);
                Ok(())
            })
            .unwrap();
        assert_eq!(store.load().unwrap().state, WriterAuthorityState::Active);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn handoff_cancel_and_destination_failure_have_distinct_safe_landings() {
        let root = root();
        let workspace = "2".repeat(64);
        let uid = crate::darwin::DarwinSystem.current_uid();
        WriterStore::initialize_layout(&root, &workspace, uid).unwrap();
        let store = WriterStore::new(&root, &workspace, uid);
        let source = Uuid::now_v7();
        let destination = Uuid::now_v7();
        let acquire = Uuid::now_v7();
        store
            .transact(|record| {
                let (_, generation) = record.prepare_acquire(source, holder(source), acquire)?;
                record.commit_acquire(acquire, generation)
            })
            .unwrap();
        let prepared = WriterHandoff {
            handoff_id: Uuid::now_v7(),
            source_run_id: source,
            destination_run_id: destination,
            expected_writer_generation: 1,
            expires_at_unix_seconds: u64::MAX,
            controller_id: Uuid::now_v7(),
            controller_generation: 1,
            source_retirement_started: false,
            source_retired: false,
        };
        store
            .transact_handoff(|record| record.prepare_handoff(prepared.clone()))
            .unwrap();
        store
            .transact_handoff(|record| record.cancel_handoff(prepared.handoff_id))
            .unwrap();
        assert_eq!(store.load().unwrap().state, WriterAuthorityState::Active);
        let reopened = WriterStore::new(&root, &workspace, uid);
        reopened
            .transact_handoff(|record| record.cancel_handoff(prepared.handoff_id))
            .unwrap();
        assert_eq!(
            reopened.load().unwrap().cancelled_handoff,
            Some(prepared.clone())
        );
        assert!(
            reopened
                .transact_handoff(|record| record.cancel_handoff(Uuid::now_v7()))
                .is_err()
        );

        store
            .transact_handoff(|record| record.prepare_handoff(prepared.clone()))
            .unwrap();
        store
            .transact_handoff(|record| record.begin_handoff_source_retirement(prepared.handoff_id))
            .unwrap();
        store
            .transact_handoff(|record| record.cancel_handoff(prepared.handoff_id))
            .unwrap();
        let uncertain = store.load().unwrap();
        assert_eq!(uncertain.state, WriterAuthorityState::BlockedUnknown);
        assert_eq!(
            uncertain.recovery_action.as_deref(),
            Some("reverify_source_writer_policy")
        );

        store
            .transact(|record| {
                record.state = WriterAuthorityState::Active;
                record.transaction_id = None;
                record.recovery_action = None;
                Ok(())
            })
            .unwrap();
        store
            .transact_handoff(|record| record.prepare_handoff(prepared.clone()))
            .unwrap();
        let (_, destination_generation) = store
            .transact_handoff(|record| {
                record.reserve_handoff_destination(prepared.handoff_id, 1, holder(destination))
            })
            .unwrap();
        assert_eq!(destination_generation, 2);
        store
            .transact_handoff(|record| {
                record.cancel_acquire(prepared.handoff_id);
                Ok(())
            })
            .unwrap();
        let landed = store.load().unwrap();
        assert_eq!(landed.state, WriterAuthorityState::None);
        assert_eq!(landed.writer_generation, 2);
        assert!(landed.holder.is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
