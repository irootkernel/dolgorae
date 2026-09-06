//! Per-Run worker discovery and the private CLI-to-worker protocol.
//!
//! This module deliberately keeps process supervision behind checked data
//! contracts.  The CLI and hidden worker share these routines, so neither side
//! can silently relax identity, framing, or runtime-record validation.

use crate::app_server::{DuplexConnection, SolicitedSink, TransportError};
#[cfg(target_os = "macos")]
use crate::conformance::ConformantLedger;
use crate::controller::{
    CredentialCarrier, authorize_controller, load_reconciled_controller_binding,
};
use crate::darwin::DarwinSystem;
use crate::domain::AggregateKind;
use crate::engagement::EngagementStore;
use crate::event::{EventDelivery, EventProjection};
use crate::fault::{FaultInjector, NoFaults};
use crate::ledger::{Ledger, LedgerClock, SystemLedgerClock};
use crate::machine::MachineError;
use crate::run::{AggregateMemberKind, RunStore};
use crate::turn::{
    AcceptedTurn, AppServer, CoordinatorConfig, CoordinatorState, DeliveryMode, DeliveryResult,
    ForeignDiagnostics, ForeignLane, IgnoredForeignRequest, ImageDetail, ImageSnapshot,
    Interaction, ResponseArtifactStore, SessionSafetyPolicy, SharedLedgerJournal, StoredArtifact,
    TerminalTurn, ThreadAttach, TransportStage, TurnCoordinator, TurnError, TurnFailureContext,
    TurnRequest, recorded_terminal,
};
use crate::workspace::SystemWorkspacePlatform;
use data_encoding::{BASE32_NOPAD, HEXLOWER};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Write};
use std::os::fd::RawFd;
use std::os::unix::fs::{
    DirBuilderExt as _, FileExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _,
    PermissionsExt as _,
};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Child;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const WORKER_PROTOCOL_VERSION: u32 = 1;
pub const CONTROL_PROTOCOL_VERSION: u32 = 1;
pub const MAX_CLI_WORKER_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const STARTUP_SLOT_BYTES: usize = 4096;
pub const STARTUP_LOCK_BYTES: usize = STARTUP_SLOT_BYTES * 2;
pub const STARTUP_HANDOFF_FD: i32 = 3;
pub const MAX_STARTUP_HANDOFF_BYTES: usize = 64 * 1024;
/// One observer page of durable events, bounded so a reply always fits inside
/// the CLI/worker frame limit.
pub const MAX_CONTROL_EVENT_PAGE: usize = 256;
/// How many app-server messages the drain may carry over while a request of
/// its own is still waiting for the reply it was issued for.
pub const MAX_CARRIED_MESSAGES: usize = 4096;
/// How many mutations may wait on the drain before a Run is refusing work
/// rather than merely busy.
pub const MAX_QUEUED_MUTATIONS: usize = 256;
/// A control call is one request and one reply; a caller that waits longer than
/// this reconnects instead of holding a worker thread open.
///
/// This bounds a *mutation*: queueing work for the drain and being told what it
/// did.  It deliberately does not bound waiting for a Turn, which is the one
/// control call whose whole purpose is to outlast a single exchange.
pub const CONTROL_CALL_TIMEOUT: Duration = Duration::from_secs(900);
/// The longest a caller may wait on one Turn inside a single control call.
///
/// SPEC-006 has a caller-supplied timeout "return the current nonterminal
/// state without interrupting the worker", which only means anything if the
/// worker actually waited that long: silently shortening an hour to fifteen
/// minutes reports a Turn as still running that the caller was never told it
/// stopped waiting for.  A caller that named no timeout asked to wait for the
/// Turn, so it waits too.  The ceiling exists because a wait is still a held
/// worker thread and a Turn that has run for a day is not going to be answered
/// by waiting a second one.
pub const MAX_TURN_WAIT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
/// How long shutdown waits for the terminal history of the Turn it interrupted.
///
/// docs/specs/README.md fixes it: an identity-verified `shutdown`, and worker `SIGTERM`,
/// "waits up to five seconds for a terminal event ... and records
/// `outcome_unknown` on expiry".
pub const SHUTDOWN_TERMINAL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a fenced controller reset waits for the drain to reach its
/// question.
///
/// It is deliberately far shorter than a Turn's budget: the reset has already
/// written its durable prepare, so an answer that never comes is a rollback
/// and a retry, not a reset that hangs behind somebody else's Turn.  It
/// matches the normative transient-reconciliation budget.
pub const RESET_FENCE_TIMEOUT: Duration = Duration::from_secs(120);
/// Binding a socket and writing a runtime record is pure local setup, so a
/// worker that has not reported `Bound` within this budget is stuck.
pub const STARTUP_BOUND_TIMEOUT: Duration = Duration::from_secs(10);
/// Reaching `Ready` additionally replays the durable ledger and connects the
/// app-server, whose cost grows with Run history.  Holding it to the bind
/// budget would kill healthy workers with long histories, so it gets its own.
pub const STARTUP_READY_TIMEOUT: Duration = Duration::from_secs(330);

const SOCKET_DOMAIN: &[u8] = b"dolgorae-socket-v1\0";
const OWNER_RECORD_DOMAIN: &[u8] = b"dolgorae-startup-owner-v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerIdentity {
    pub workspace_id: String,
    pub run_id: Uuid,
    pub run_generation: u64,
    pub boot_uuid: Uuid,
    pub pid: u32,
    pub process_group_id: u32,
    pub session_id: u32,
    pub uid: u32,
    pub start_tvsec: u64,
    pub start_tvusec: u64,
    pub executable_path: PathBuf,
    pub executable_device: u64,
    pub executable_inode: u64,
    pub executable_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerBootstrap {
    pub schema_version: u32,
    pub workspace_id: String,
    pub run_id: Uuid,
    pub run_generation: u64,
    pub boot_uuid: Uuid,
    pub executable_sha256: String,
    pub executable_path_sha256: String,
    pub dolgorae_version: String,
    pub mutation_protocol_version: u32,
    pub control_socket_epoch: u64,
    /// The Runtime Profile this Run is pinned to.
    ///
    /// It is carried because a compatibility refusal has to name the profile
    /// it was rejected against, and the worker never re-derives which profile
    /// a Run belongs to.
    pub profile: String,
    /// The workspace state root this Run's durable authority lives under.
    ///
    /// It is carried rather than derived so the worker never re-computes which
    /// Controller binding it is allowed to trust.
    pub state_root: PathBuf,
    pub ledger_root: PathBuf,
    pub runtime_record_path: PathBuf,
    pub startup_lock_path: PathBuf,
    /// Absent for an identity-only worker; present once the Run owns a real
    /// app-server conversation.
    #[serde(default)]
    pub session: Option<WorkerSessionBootstrap>,
}

impl WorkerBootstrap {
    pub fn validate(&self) -> Result<(), WorkerProtocolError> {
        let record_name = format!("{}.json", self.run_id);
        let lock_name = format!("{}.lock", self.run_id);
        if self.schema_version != 1
            || decode_sha256(&self.workspace_id).is_none()
            || self.run_id.get_version_num() != 7
            || self.boot_uuid.is_nil()
            || decode_sha256(&self.executable_sha256).is_none()
            || decode_sha256(&self.executable_path_sha256).is_none()
            || self.dolgorae_version.is_empty()
            || self.dolgorae_version.len() > 128
            || self.mutation_protocol_version == 0
            || self.control_socket_epoch == 0
            || !self.state_root.is_absolute()
            || self.ledger_root != self.state_root.join("runs").join(self.run_id.to_string())
            || !self.ledger_root.is_absolute()
            || !self.runtime_record_path.is_absolute()
            || !self.startup_lock_path.is_absolute()
            || self
                .runtime_record_path
                .file_name()
                .and_then(|value| value.to_str())
                != Some(record_name.as_str())
            || self
                .startup_lock_path
                .file_name()
                .and_then(|value| value.to_str())
                != Some(lock_name.as_str())
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        if let Some(session) = &self.session {
            session.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StartupHandoff {
    Bound { record: WorkerRuntimeRecord },
    Ready { record: WorkerRuntimeRecord },
    Failed { code: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventStreamEnd {
    pub schema_version: u32,
    pub kind: String,
    pub next_cursor: String,
}

#[cfg(target_os = "macos")]
pub struct StartedWorker {
    pub child: Child,
    pub record: WorkerRuntimeRecord,
}

impl WorkerIdentity {
    pub fn validate(&self) -> Result<(), WorkerProtocolError> {
        if decode_sha256(&self.workspace_id).is_none()
            || self.run_id.get_version_num() != 7
            || self.boot_uuid.is_nil()
            || self.pid == 0
            || self.process_group_id == 0
            || self.session_id == 0
            || self.start_tvsec == 0
            || !self.executable_path.is_absolute()
            || self.executable_device == 0
            || self.executable_inode == 0
            || decode_sha256(&self.executable_sha256).is_none()
        {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn current_worker_identity(
    bootstrap: &WorkerBootstrap,
    process: crate::providers::ProcessIdentity,
) -> Result<WorkerIdentity, WorkerProtocolError> {
    current_process_identity(
        &bootstrap.workspace_id,
        bootstrap.run_id,
        bootstrap.run_generation,
        bootstrap.boot_uuid,
        process,
        Some(&bootstrap.executable_sha256),
    )
    .map(|(identity, _)| identity)
}

#[cfg(target_os = "macos")]
fn current_process_identity(
    workspace_id: &str,
    run_id: Uuid,
    run_generation: u64,
    boot_uuid: Uuid,
    process: crate::providers::ProcessIdentity,
    expected_executable_sha256: Option<&str>,
) -> Result<(WorkerIdentity, String), WorkerProtocolError> {
    let before = DarwinSystem
        .bsd_process_identity(process.pid)
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    if before.zombie
        || before.pid != process.pid
        || before.uid != process.uid
        || before.process_group_id != process.process_group_id
    {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let executable_path = DarwinSystem
        .realpath(&std::env::current_exe().map_err(|_| WorkerProtocolError::Io)?)
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    let metadata = fs::metadata(&executable_path).map_err(|_| WorkerProtocolError::Io)?;
    let executable_sha256 = file_sha256(&executable_path)?;
    let after = DarwinSystem
        .bsd_process_identity(process.pid)
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    if before != after
        || expected_executable_sha256.is_some_and(|expected| executable_sha256 != expected)
    {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let executable_path_sha256 =
        HEXLOWER.encode(Sha256::digest(executable_path.as_os_str().as_encoded_bytes()).as_slice());
    Ok((
        WorkerIdentity {
            workspace_id: workspace_id.to_owned(),
            run_id,
            run_generation,
            boot_uuid,
            pid: process.pid,
            process_group_id: process.process_group_id,
            session_id: before.session_id,
            uid: process.uid,
            start_tvsec: before.start_tvsec,
            start_tvusec: before.start_tvusec,
            executable_path,
            executable_device: metadata.dev(),
            executable_inode: metadata.ino(),
            executable_sha256,
        },
        executable_path_sha256,
    ))
}

#[cfg(target_os = "macos")]
pub fn current_startup_owner(
    workspace_id: &str,
    run_id: Uuid,
    run_generation: u64,
) -> Result<StartupOwnerRecord, WorkerProtocolError> {
    let uid = DarwinSystem.current_uid();
    let (identity, executable_path_sha256) = current_process_identity(
        workspace_id,
        run_id,
        run_generation,
        boot_session_uuid(uid)?,
        DarwinSystem
            .current_process()
            .map_err(|_| WorkerProtocolError::InvalidIdentity)?,
        None,
    )?;
    Ok(StartupOwnerRecord {
        schema_version: 1,
        slot: 0,
        identity,
        executable_path_sha256,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocketIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRuntimeRecord {
    pub schema_version: u32,
    pub identity: WorkerIdentity,
    pub socket_path: PathBuf,
    pub socket_identity: SocketIdentity,
    pub control_socket_epoch: u64,
    #[serde(default)]
    pub app_server_epoch: Option<u64>,
    #[serde(default)]
    pub dedicated_server_identity: Option<DedicatedServerIdentity>,
    pub dolgorae_version: String,
    pub mutation_protocol_version: u32,
    pub binary_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DedicatedServerIdentity {
    pub pid: u32,
    pub process_group_id: u32,
    pub session_id: u32,
    pub uid: u32,
    pub start_tvsec: u64,
    pub start_tvusec: u64,
    pub executable_path: PathBuf,
    pub executable_device: u64,
    pub executable_inode: u64,
    pub executable_sha256: String,
}

impl DedicatedServerIdentity {
    fn validate(&self) -> Result<(), WorkerProtocolError> {
        if self.pid == 0
            || self.process_group_id == 0
            || self.session_id == 0
            || !self.executable_path.is_absolute()
            || self.executable_device == 0
            || self.executable_inode == 0
            || decode_sha256(&self.executable_sha256).is_none()
        {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessIdentityVerdict {
    Absent,
    Mismatch,
    Match,
    Unverifiable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundAbsenceEvidence {
    pub census_revision: u64,
    pub consecutive_empty_samples: u8,
}

/// Prove that a live worker's process group has no workload descendants.
///
/// The worker itself is the sole allowed member. Every sample revalidates the
/// full recorded identity before trusting the PGID, and any census error or
/// additional member fails closed without signalling it.
pub fn prove_worker_workload_absent(
    record: &WorkerRuntimeRecord,
) -> Result<BackgroundAbsenceEvidence, WorkerProtocolError> {
    prove_group_empty(record, true, false)
}

/// Prove that a recorded worker generation and its entire process group are
/// absent. This is the stronger recovery/reset predicate: a matching live
/// leader is not absence, while PID reuse or unreadable identity is never
/// silently treated as safe.
pub fn prove_worker_generation_absent(
    record: &WorkerRuntimeRecord,
) -> Result<BackgroundAbsenceEvidence, WorkerProtocolError> {
    prove_group_empty(record, false, false)
}

fn prove_group_empty(
    record: &WorkerRuntimeRecord,
    allow_recorded_worker: bool,
    allow_recorded_dedicated_server: bool,
) -> Result<BackgroundAbsenceEvidence, WorkerProtocolError> {
    let expected = if allow_recorded_worker {
        ProcessIdentityVerdict::Match
    } else {
        ProcessIdentityVerdict::Absent
    };
    for sample in 0..5_u8 {
        if classify_worker_identity(record) != expected {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        if let Some(server) = &record.dedicated_server_identity {
            let expected_server = if allow_recorded_worker || allow_recorded_dedicated_server {
                ProcessIdentityVerdict::Match
            } else {
                ProcessIdentityVerdict::Absent
            };
            if classify_dedicated_server_identity(server) != expected_server
                || !dedicated_group_has_only_expected_leader(
                    server,
                    allow_recorded_worker || allow_recorded_dedicated_server,
                )?
            {
                return Err(WorkerProtocolError::InvalidIdentity);
            }
        }
        let mut members = DarwinSystem
            .process_group_pids(record.identity.process_group_id)
            .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
        if !allow_recorded_worker {
            members.retain(|pid| *pid != record.identity.pid);
        }
        let census = DarwinSystem
            .all_process_identities()
            .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
        let mut enumerated_group = census
            .iter()
            .filter(|identity| {
                identity.process_group_id == record.identity.process_group_id
                    && (allow_recorded_worker || identity.pid != record.identity.pid)
            })
            .map(|identity| identity.pid)
            .collect::<Vec<_>>();
        enumerated_group.sort_unstable();
        if enumerated_group != members {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        let by_pid = census
            .iter()
            .map(|identity| (identity.pid, identity.parent_pid))
            .collect::<std::collections::BTreeMap<_, _>>();
        let unexpected = census.iter().any(|identity| {
            if allow_recorded_worker && identity.pid == record.identity.pid {
                return false;
            }
            if record
                .dedicated_server_identity
                .as_ref()
                .is_some_and(|server| identity.pid == server.pid)
            {
                return false;
            }
            if !allow_recorded_worker
                && identity.zombie
                && identity.pid == record.identity.pid
                && identity.uid == record.identity.uid
                && identity.process_group_id == record.identity.process_group_id
                && identity.session_id == record.identity.session_id
                && identity.start_tvsec == record.identity.start_tvsec
                && identity.start_tvusec == record.identity.start_tvusec
            {
                return false;
            }
            let same_scope = process_is_in_scope(
                identity,
                record.identity.pid,
                record.identity.process_group_id,
                record.identity.session_id,
                &by_pid,
            );
            same_scope || (!allow_recorded_worker && identity.pid == record.identity.pid)
        });
        if unexpected {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        if sample < 4 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(BackgroundAbsenceEvidence {
        census_revision: record.control_socket_epoch,
        consecutive_empty_samples: 5,
    })
}

fn dedicated_group_has_only_expected_leader(
    server: &DedicatedServerIdentity,
    allow_leader: bool,
) -> Result<bool, WorkerProtocolError> {
    let mut members = DarwinSystem
        .process_group_pids(server.process_group_id)
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    if !allow_leader {
        members.retain(|pid| *pid != server.pid);
    }
    let census = DarwinSystem
        .all_process_identities()
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    let mut enumerated = census
        .iter()
        .filter(|identity| {
            identity.process_group_id == server.process_group_id
                && (allow_leader || identity.pid != server.pid)
        })
        .map(|identity| identity.pid)
        .collect::<Vec<_>>();
    enumerated.sort_unstable();
    if enumerated != members {
        return Ok(false);
    }
    let by_pid = census
        .iter()
        .map(|identity| (identity.pid, identity.parent_pid))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(!census.iter().any(|identity| {
        if allow_leader && identity.pid == server.pid {
            return false;
        }
        if !allow_leader
            && identity.zombie
            && identity.pid == server.pid
            && identity.uid == server.uid
            && identity.process_group_id == server.process_group_id
            && identity.session_id == server.session_id
            && identity.start_tvsec == server.start_tvsec
            && identity.start_tvusec == server.start_tvusec
        {
            return false;
        }
        process_is_in_scope(
            identity,
            server.pid,
            server.process_group_id,
            server.session_id,
            &by_pid,
        )
    }))
}

#[cfg(target_os = "macos")]
#[must_use]
pub fn classify_dedicated_server_identity(
    identity: &DedicatedServerIdentity,
) -> ProcessIdentityVerdict {
    if identity.validate().is_err() {
        return ProcessIdentityVerdict::Unverifiable;
    }
    let watch = match DarwinSystem.watch_process_exit(identity.pid) {
        Ok(watch) => watch,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            return ProcessIdentityVerdict::Absent;
        }
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    let before = match DarwinSystem.bsd_process_identity(identity.pid) {
        Ok(value) if value.zombie => return ProcessIdentityVerdict::Absent,
        Ok(value) => value,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            return ProcessIdentityVerdict::Absent;
        }
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    if before.pid != identity.pid
        || before.uid != identity.uid
        || before.process_group_id != identity.process_group_id
        || before.session_id != identity.session_id
        || before.start_tvsec != identity.start_tvsec
        || before.start_tvusec != identity.start_tvusec
    {
        return ProcessIdentityVerdict::Mismatch;
    }
    let mut executable = match File::open(&identity.executable_path) {
        Ok(file) => file,
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    let metadata = match executable.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    let digest = match file_sha256_reader(&mut executable) {
        Ok(digest) => digest,
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    let after = match DarwinSystem.bsd_process_identity(identity.pid) {
        Ok(value) => value,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            return ProcessIdentityVerdict::Absent;
        }
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    if after != before {
        return ProcessIdentityVerdict::Unverifiable;
    }
    match watch.exited() {
        Ok(true) => return ProcessIdentityVerdict::Absent,
        Ok(false) => {}
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    }
    if metadata.dev() != identity.executable_device
        || metadata.ino() != identity.executable_inode
        || digest != identity.executable_sha256
    {
        ProcessIdentityVerdict::Mismatch
    } else {
        ProcessIdentityVerdict::Match
    }
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn classify_dedicated_server_identity(
    _identity: &DedicatedServerIdentity,
) -> ProcessIdentityVerdict {
    ProcessIdentityVerdict::Unverifiable
}

fn process_descends_from(
    mut pid: u32,
    ancestor: u32,
    parents: &std::collections::BTreeMap<u32, u32>,
) -> bool {
    for _ in 0..256 {
        let Some(parent) = parents.get(&pid).copied() else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        if parent <= 1 || parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

fn process_is_in_scope(
    identity: &crate::darwin::BsdProcessIdentity,
    leader_pid: u32,
    process_group_id: u32,
    session_id: u32,
    parents: &std::collections::BTreeMap<u32, u32>,
) -> bool {
    identity.process_group_id == process_group_id
        || identity.session_id == session_id
        || process_descends_from(identity.pid, leader_pid, parents)
}

impl ProcessIdentityVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "Absent",
            Self::Mismatch => "Mismatch",
            Self::Match => "Match",
            Self::Unverifiable => "Unverifiable",
        }
    }
}

#[cfg(target_os = "macos")]
#[must_use]
pub fn classify_worker_identity(record: &WorkerRuntimeRecord) -> ProcessIdentityVerdict {
    if record.validate().is_err() {
        return ProcessIdentityVerdict::Unverifiable;
    }
    let current_boot = match DarwinSystem.boot_session_uuid() {
        Ok(value) => value,
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    if Uuid::parse_str(&current_boot).ok() != Some(record.identity.boot_uuid) {
        return ProcessIdentityVerdict::Absent;
    }
    let exit_watch = match DarwinSystem.watch_process_exit(record.identity.pid) {
        Ok(watch) => watch,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            return ProcessIdentityVerdict::Absent;
        }
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    match exit_watch.exited() {
        Ok(true) => return ProcessIdentityVerdict::Absent,
        Ok(false) => {}
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    }
    let before = match DarwinSystem.bsd_process_identity(record.identity.pid) {
        Ok(value) if value.zombie => return ProcessIdentityVerdict::Absent,
        Ok(value) => value,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            return ProcessIdentityVerdict::Absent;
        }
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    if before.pid != record.identity.pid
        || before.uid != record.identity.uid
        || before.process_group_id != record.identity.process_group_id
        || before.session_id != record.identity.session_id
        || before.start_tvsec != record.identity.start_tvsec
        || before.start_tvusec != record.identity.start_tvusec
    {
        return ProcessIdentityVerdict::Mismatch;
    }
    let mut executable = match File::open(&record.identity.executable_path) {
        Ok(file) => file,
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    let metadata = match executable.metadata() {
        Ok(value) => value,
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    let digest = match file_sha256_reader(&mut executable) {
        Ok(value) => value,
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    let after = match DarwinSystem.bsd_process_identity(record.identity.pid) {
        Ok(value) => value,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            return ProcessIdentityVerdict::Absent;
        }
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    };
    if after != before {
        return ProcessIdentityVerdict::Unverifiable;
    }
    match exit_watch.exited() {
        Ok(true) => return ProcessIdentityVerdict::Absent,
        Ok(false) => {}
        Err(_) => return ProcessIdentityVerdict::Unverifiable,
    }
    if metadata.dev() != record.identity.executable_device
        || metadata.ino() != record.identity.executable_inode
        || digest != record.identity.executable_sha256
    {
        ProcessIdentityVerdict::Mismatch
    } else {
        ProcessIdentityVerdict::Match
    }
}

#[derive(Debug)]
pub struct WorkerSocketLease {
    listener: UnixListener,
    path: PathBuf,
    identity: SocketIdentity,
}

impl WorkerSocketLease {
    #[must_use]
    pub const fn listener(&self) -> &UnixListener {
        &self.listener
    }

    #[must_use]
    pub const fn identity(&self) -> &SocketIdentity {
        &self.identity
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkerSocketLease {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.identity.device
            && metadata.ino() == self.identity.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug)]
pub struct VerifiedStaleSocket {
    path: PathBuf,
    identity: SocketIdentity,
    _prior_generation_absent: (),
}

impl VerifiedStaleSocket {
    /// The process-identity prober is the only production caller intended to
    /// mint this capability.  Merely observing an old pathname is insufficient.
    #[allow(
        dead_code,
        reason = "TASK-020 process identity and group recovery mints this sealed capability"
    )]
    pub(crate) fn from_absent_generation(
        record: &WorkerRuntimeRecord,
        observed: SocketIdentity,
    ) -> Result<Self, WorkerProtocolError> {
        record.validate()?;
        if record.socket_identity != observed {
            return Err(WorkerProtocolError::SocketIdentityMismatch);
        }
        Ok(Self {
            path: record.socket_path.clone(),
            identity: observed,
            _prior_generation_absent: (),
        })
    }
}

#[derive(Debug)]
pub struct StartupLockFile {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(target_os = "macos")]
pub struct StartupByteGuard<'a> {
    lock: &'a StartupLockFile,
    slot: u8,
    released: bool,
}

impl StartupLockFile {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open(path: &Path, uid: u32) -> Result<Self, WorkerProtocolError> {
        let parent = path
            .parent()
            .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
        let parent_metadata = fs::symlink_metadata(parent).map_err(|_| WorkerProtocolError::Io)?;
        if !parent_metadata.file_type().is_dir()
            || parent_metadata.uid() != uid
            || parent_metadata.mode() & 0o777 != 0o700
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        let created = !path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| WorkerProtocolError::Io)?;
        if created {
            file.set_len(STARTUP_LOCK_BYTES as u64)
                .map_err(|_| WorkerProtocolError::Io)?;
            file.sync_all().map_err(|_| WorkerProtocolError::Io)?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| WorkerProtocolError::Io)?;
        }
        let metadata = file.metadata().map_err(|_| WorkerProtocolError::Io)?;
        let path_metadata = fs::symlink_metadata(path).map_err(|_| WorkerProtocolError::Io)?;
        if !metadata.file_type().is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() != STARTUP_LOCK_BYTES as u64
            || metadata.dev() != path_metadata.dev()
            || metadata.ino() != path_metadata.ino()
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        Ok(Self {
            file,
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub fn read_owner(&self, slot: u8) -> Result<Option<StartupOwnerRecord>, WorkerProtocolError> {
        let offset = slot_offset(slot)?;
        let mut bytes = [0_u8; STARTUP_SLOT_BYTES];
        self.file
            .read_exact_at(&mut bytes, offset)
            .map_err(|_| WorkerProtocolError::Io)?;
        StartupOwnerRecord::decode_slot(&bytes)
    }

    pub fn write_owner(&self, record: &StartupOwnerRecord) -> Result<(), WorkerProtocolError> {
        self.revalidate()?;
        let bytes = record.encode_slot()?;
        self.file
            .write_all_at(&bytes, slot_offset(record.slot)?)
            .map_err(|_| WorkerProtocolError::Io)?;
        self.file.sync_all().map_err(|_| WorkerProtocolError::Io)
    }

    pub fn clear_owner(&self, slot: u8) -> Result<(), WorkerProtocolError> {
        self.revalidate()?;
        self.file
            .write_all_at(&[0_u8; STARTUP_SLOT_BYTES], slot_offset(slot)?)
            .map_err(|_| WorkerProtocolError::Io)?;
        self.file.sync_all().map_err(|_| WorkerProtocolError::Io)
    }

    #[cfg(target_os = "macos")]
    pub fn claim(
        &self,
        owner: &StartupOwnerRecord,
        timeout: Duration,
    ) -> Result<StartupByteGuard<'_>, WorkerProtocolError> {
        self.revalidate()?;
        let offset = i64::try_from(slot_offset(owner.slot)?)
            .map_err(|_| WorkerProtocolError::InvalidOwnerRecord)?;
        let acquired = if timeout.is_zero() {
            crate::darwin::DarwinSystem.try_lock_byte(&self.file, offset)
        } else {
            crate::darwin::DarwinSystem.lock_byte_timeout(&self.file, offset, timeout)
        };
        acquired.map_err(|_| WorkerProtocolError::StartupBusy)?;
        if let Err(error) = self.write_owner(owner) {
            let _ = crate::darwin::DarwinSystem.unlock_byte(&self.file, offset);
            return Err(error);
        }
        Ok(StartupByteGuard {
            lock: self,
            slot: owner.slot,
            released: false,
        })
    }

    /// Take the Run's startup/mutation range (slot 0) for an operation that
    /// serializes *against* worker startup rather than performing one.
    ///
    /// No owner record is written. docs/specs/README.md: "All-zero, stale,
    /// checksum-invalid, or unknown-layout slots never establish identity and
    /// do not block a kernel-lock winner; a locked range with no valid
    /// matching record is `Unverifiable`" — and `Unverifiable` returns
    /// `RUN_BUSY`, which is exactly the answer a start contender should get
    /// while an operator controller reset holds the range.  The caller keeps
    /// this `StartupLockFile` alive for the whole operation: POSIX record
    /// locks are per-process and closing any descriptor for the file would
    /// drop them all.
    #[cfg(target_os = "macos")]
    pub fn hold_startup_range(&self, timeout: Duration) -> Result<(), WorkerProtocolError> {
        self.revalidate()?;
        let offset = startup_range_offset()?;
        let acquired = if timeout.is_zero() {
            crate::darwin::DarwinSystem.try_lock_byte(&self.file, offset)
        } else {
            crate::darwin::DarwinSystem.lock_byte_timeout(&self.file, offset, timeout)
        };
        acquired.map_err(|_| WorkerProtocolError::StartupBusy)
    }

    /// Release the startup/mutation range before external work, as PREPARE
    /// and COMMIT each require.
    #[cfg(target_os = "macos")]
    pub fn release_startup_range(&self) -> Result<(), WorkerProtocolError> {
        let offset = startup_range_offset()?;
        let clear = self.clear_owner(0);
        let unlock = crate::darwin::DarwinSystem
            .unlock_byte(&self.file, offset)
            .map_err(|_| WorkerProtocolError::Io);
        clear.and(unlock)
    }

    fn revalidate(&self) -> Result<(), WorkerProtocolError> {
        let descriptor = self.file.metadata().map_err(|_| WorkerProtocolError::Io)?;
        let pathname = fs::symlink_metadata(&self.path)
            .map_err(|_| WorkerProtocolError::SocketIdentityMismatch)?;
        if descriptor.dev() != self.device
            || descriptor.ino() != self.inode
            || pathname.dev() != self.device
            || pathname.ino() != self.inode
        {
            return Err(WorkerProtocolError::SocketIdentityMismatch);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl StartupByteGuard<'_> {
    pub fn release(mut self) -> Result<(), WorkerProtocolError> {
        self.lock.clear_owner(self.slot)?;
        let offset = i64::try_from(slot_offset(self.slot)?)
            .map_err(|_| WorkerProtocolError::InvalidOwnerRecord)?;
        crate::darwin::DarwinSystem
            .unlock_byte(&self.lock.file, offset)
            .map_err(|_| WorkerProtocolError::Io)?;
        self.released = true;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Drop for StartupByteGuard<'_> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let _ = self.lock.clear_owner(self.slot);
        if let Ok(offset) = slot_offset(self.slot).and_then(|value| {
            i64::try_from(value).map_err(|_| WorkerProtocolError::InvalidOwnerRecord)
        }) {
            let _ = crate::darwin::DarwinSystem.unlock_byte(&self.lock.file, offset);
        }
    }
}

impl WorkerRuntimeRecord {
    pub fn validate(&self) -> Result<(), WorkerProtocolError> {
        self.identity.validate()?;
        if self.schema_version != 1
            || self.control_socket_epoch == 0
            || self.app_server_epoch == Some(0)
            || self.socket_identity.device == 0
            || self.socket_identity.inode == 0
            || self.dolgorae_version.is_empty()
            || self.dolgorae_version.len() > 128
            || self.mutation_protocol_version == 0
            || decode_sha256(&self.binary_sha256).is_none()
            || self.socket_path
                != worker_socket_path(
                    self.identity.uid,
                    &self.identity.workspace_id,
                    self.identity.run_id,
                )?
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        if let Some(identity) = &self.dedicated_server_identity {
            identity.validate()?;
        }
        Ok(())
    }

    /// Refuse an ordinary request when the worker answering this Run is not
    /// this Dolgorae build.
    ///
    /// The three fields are compared together because any one of them drifting
    /// means the worker's mutation semantics are not the ones the caller was
    /// compiled against.
    pub fn validate_ordinary_peer(
        &self,
        current: &ExecutingBuild,
    ) -> Result<(), WorkerProtocolError> {
        if self.dolgorae_version != current.version
            || self.mutation_protocol_version != current.mutation_protocol_version
            || self.binary_sha256 != current.binary_sha256
        {
            return Err(WorkerProtocolError::ProtocolMismatch {
                expected_protocol: current.mutation_protocol_version,
                actual_protocol: self.mutation_protocol_version,
            });
        }
        Ok(())
    }

    pub fn observe_socket(&self) -> Result<SocketIdentity, WorkerProtocolError> {
        self.validate()?;
        let metadata = fs::symlink_metadata(&self.socket_path)
            .map_err(|_| WorkerProtocolError::SocketIdentityMismatch)?;
        if !metadata.file_type().is_socket() || metadata.uid() != self.identity.uid {
            return Err(WorkerProtocolError::SocketIdentityMismatch);
        }
        let observed = SocketIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if observed != self.socket_identity {
            return Err(WorkerProtocolError::SocketIdentityMismatch);
        }
        Ok(observed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerHello {
    pub schema_version: u32,
    pub identity: WorkerIdentity,
    pub control_socket_epoch: u64,
    pub dolgorae_version: String,
    pub mutation_protocol_version: u32,
    pub binary_sha256: String,
}

impl WorkerHello {
    pub fn validate_ordinary(&self, expected: &Self) -> Result<(), WorkerProtocolError> {
        self.validate_identity(expected)?;
        if self.dolgorae_version != expected.dolgorae_version
            || self.mutation_protocol_version != expected.mutation_protocol_version
            || self.binary_sha256 != expected.binary_sha256
        {
            return Err(self.protocol_mismatch(expected));
        }
        Ok(())
    }

    /// Control v1 remains usable across a Dolgorae binary or mutation-protocol
    /// upgrade, but never across worker identity or socket-epoch drift.
    pub fn validate_control_v1(&self, expected: &Self) -> Result<(), WorkerProtocolError> {
        self.validate_identity(expected)
    }

    fn validate_identity(&self, expected: &Self) -> Result<(), WorkerProtocolError> {
        self.identity.validate()?;
        expected.identity.validate()?;
        if self.schema_version != 1
            || expected.schema_version != 1
            || self.control_socket_epoch == 0
            || self.identity != expected.identity
            || self.control_socket_epoch != expected.control_socket_epoch
        {
            return Err(self.protocol_mismatch(expected));
        }
        Ok(())
    }

    /// The mismatch this hello reports against the one it was checked with.
    fn protocol_mismatch(&self, expected: &Self) -> WorkerProtocolError {
        WorkerProtocolError::ProtocolMismatch {
            expected_protocol: expected.mutation_protocol_version,
            actual_protocol: self.mutation_protocol_version,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlRequestV1 {
    Hello {
        expected: WorkerIdentity,
    },
    Status {
        expected: WorkerIdentity,
    },
    Shutdown {
        expected: WorkerIdentity,
    },
    /// Read the Run's control state together with the terminal its drain
    /// observed.  Open to any same-uid observer.
    ///
    /// docs/specs/README.md sends a Master to `run status.data.last_terminal` for the
    /// response, usage, and cursor behind an intentionally minimal exit-7
    /// envelope, and SPEC-007 freezes `status` byte-identically across builds.
    /// Those two cannot both ride one request: the frozen answer may not grow
    /// a member, so the terminal travels on this ordinary, skew-checked
    /// request instead, and only a caller of this worker's own build ever sees
    /// the wider answer.
    RunStatus {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
    },
    /// Start a Turn and wait for it to reach a terminal or interaction state.
    ///
    /// `timeout_ms` is the caller's own budget: SPEC-006 has a
    /// caller-supplied timeout return the current nonterminal state without
    /// interrupting the worker, so it bounds this reply and nothing else.
    Send {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        request: TurnControlRequest,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Start a Turn and return as soon as the app-server accepts it.
    Submit {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        request: TurnControlRequest,
    },
    /// Facade-private submit authorized by the owning External Specialist
    /// Engagement rather than by disclosure of the member Run credential.
    ExternalSubmit {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        engagement_id: Uuid,
        request: TurnControlRequest,
    },
    /// Rejoin one addressed Turn; a reconnecting caller uses this after a
    /// Submit.
    ///
    /// The Turn is named, never inferred: SPEC-006 has `wait` require both run
    /// and turn IDs, so a Turn that has already settled returns its own
    /// outcome and a Turn this Run never had is `TURN_NOT_FOUND` rather than
    /// whichever Turn happens to be live.
    Wait {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        turn_id: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Read the durable event ledger.  Open to any same-uid observer.
    Events {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        after: u64,
        projection: EventProjection,
        limit: usize,
    },
    Respond {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        request_id: u64,
        idempotency_key: String,
        response: Value,
    },
    Interrupt {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
    },
    ExternalInterrupt {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        engagement_id: Uuid,
    },
    Pause {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        interrupt: bool,
    },
    Resume {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
    },
    Reconcile {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
    },
    SettleLifecycleTimeout {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        close: bool,
    },
    /// End the Run.  `interrupt` is the caller's explicit authorization to
    /// interrupt live work: docs/specs/README.md refuses a running or waiting Run without
    /// it rather than interrupting implicitly.
    Close {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        interrupt: bool,
    },
    ExternalClose {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        engagement_id: Uuid,
        interrupt: bool,
    },
    SetWriterAccess {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        write: bool,
        writer_generation: u64,
        transaction_id: Uuid,
    },
    ExternalSetWriterAccess {
        expected: WorkerIdentity,
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        engagement_id: Uuid,
        write: bool,
        writer_generation: u64,
        transaction_id: Uuid,
    },
    /// Report this Run's authoritative state to an operator controller reset
    /// that has already fenced it with a durable prepare.
    ///
    /// SPEC-013 gives `run controller reset` the lock prefix "Operator, run
    /// startup/mutation", and the Run's mutation serializer lives inside this
    /// worker.  The resetting CLI cannot take it from outside, and
    /// `state.json` is a group-committed projection that lags the durable
    /// ledger, so deciding "is a Turn active" from the file can observe an
    /// idle Run the drain has already committed a Turn for.  This request is
    /// answered *by the drain*, so it is ordered against every mutation the
    /// drain performs, in both directions.
    ResetFence {
        expected: WorkerIdentity,
        /// The build this request was composed by; a worker refuses one it
        /// does not share.
        #[serde(default)]
        caller: Option<ExecutingBuild>,
        confirmation: Uuid,
    },
}

impl ControlRequestV1 {
    #[must_use]
    pub const fn expected(&self) -> &WorkerIdentity {
        match self {
            Self::Hello { expected }
            | Self::Status { expected }
            | Self::Shutdown { expected }
            | Self::RunStatus { expected, .. }
            | Self::Send { expected, .. }
            | Self::Submit { expected, .. }
            | Self::ExternalSubmit { expected, .. }
            | Self::Wait { expected, .. }
            | Self::Events { expected, .. }
            | Self::Respond { expected, .. }
            | Self::Interrupt { expected, .. }
            | Self::ExternalInterrupt { expected, .. }
            | Self::Pause { expected, .. }
            | Self::Resume { expected, .. }
            | Self::Reconcile { expected, .. }
            | Self::SettleLifecycleTimeout { expected, .. }
            | Self::Close { expected, .. }
            | Self::ExternalClose { expected, .. }
            | Self::SetWriterAccess { expected, .. }
            | Self::ExternalSetWriterAccess { expected, .. }
            | Self::ResetFence { expected, .. } => expected,
        }
    }

    /// The build the caller declared, absent on frozen control v1 and on any
    /// request composed by a build that predates the declaration.
    #[must_use]
    pub const fn caller(&self) -> Option<&ExecutingBuild> {
        match self {
            Self::Hello { .. } | Self::Status { .. } | Self::Shutdown { .. } => None,
            Self::RunStatus { caller, .. }
            | Self::Send { caller, .. }
            | Self::Submit { caller, .. }
            | Self::ExternalSubmit { caller, .. }
            | Self::Wait { caller, .. }
            | Self::Events { caller, .. }
            | Self::Respond { caller, .. }
            | Self::Interrupt { caller, .. }
            | Self::ExternalInterrupt { caller, .. }
            | Self::Pause { caller, .. }
            | Self::Resume { caller, .. }
            | Self::Reconcile { caller, .. }
            | Self::SettleLifecycleTimeout { caller, .. }
            | Self::Close { caller, .. }
            | Self::ExternalClose { caller, .. }
            | Self::SetWriterAccess { caller, .. }
            | Self::ExternalSetWriterAccess { caller, .. }
            | Self::ResetFence { caller, .. } => caller.as_ref(),
        }
    }

    /// Declare the build composing this request.
    ///
    /// Frozen control v1 keeps its exact v1 wire, so it declares nothing: a
    /// worker from another build must still answer `hello`, `status`, and
    /// `shutdown` byte-identically.
    pub fn declare_caller(&mut self, build: ExecutingBuild) {
        match self {
            Self::Hello { .. } | Self::Status { .. } | Self::Shutdown { .. } => {}
            Self::RunStatus { caller, .. }
            | Self::Send { caller, .. }
            | Self::Submit { caller, .. }
            | Self::ExternalSubmit { caller, .. }
            | Self::Wait { caller, .. }
            | Self::Events { caller, .. }
            | Self::Respond { caller, .. }
            | Self::Interrupt { caller, .. }
            | Self::ExternalInterrupt { caller, .. }
            | Self::Pause { caller, .. }
            | Self::Resume { caller, .. }
            | Self::Reconcile { caller, .. }
            | Self::SettleLifecycleTimeout { caller, .. }
            | Self::Close { caller, .. }
            | Self::ExternalClose { caller, .. }
            | Self::SetWriterAccess { caller, .. }
            | Self::ExternalSetWriterAccess { caller, .. }
            | Self::ResetFence { caller, .. } => *caller = Some(build),
        }
    }

    /// Observers never change Run state, so they are never refused for lacking
    /// mutation authority and never serialise behind an active Turn.
    #[must_use]
    pub const fn observes_only(&self) -> bool {
        matches!(
            self,
            Self::Hello { .. } | Self::Status { .. } | Self::RunStatus { .. } | Self::Events { .. }
        )
    }

    /// Requests that change Run state and therefore need an authoritative
    /// Controller credential revalidation immediately before their effect.
    ///
    /// `Wait` rejoins an already-authorized Turn without starting, answering,
    /// interrupting, or ending anything, so it stays open to same-uid
    /// observers alongside `hello`, `status`, and `events`.  `Shutdown` keeps
    /// its frozen control-v1 identity-only authorization.  `ResetFence`
    /// changes nothing and is authorized by the durable operator-written reset
    /// prepare it reports against, not by a Controller credential — the
    /// operator performing a reset is precisely the caller who may not hold
    /// one.
    #[must_use]
    pub const fn requires_controller(&self) -> bool {
        matches!(
            self,
            Self::Send { .. }
                | Self::Submit { .. }
                | Self::ExternalSubmit { .. }
                | Self::Respond { .. }
                | Self::Interrupt { .. }
                | Self::ExternalInterrupt { .. }
                | Self::Pause { .. }
                | Self::Resume { .. }
                | Self::Reconcile { .. }
                | Self::SettleLifecycleTimeout { .. }
                | Self::Close { .. }
                | Self::ExternalClose { .. }
                | Self::SetWriterAccess { .. }
                | Self::ExternalSetWriterAccess { .. }
        )
    }

    /// Whether this request belongs to the frozen control-v1 surface.
    ///
    /// SPEC-007 freezes exactly `hello`, `status`, and `shutdown`: they stay
    /// usable across a Dolgorae binary or mutation-protocol upgrade so an
    /// operator can always reach a worker from an older or newer build.
    /// Everything else is an ordinary request and must not mix builds inside
    /// one Run generation.
    #[must_use]
    pub const fn frozen_control_v1(&self) -> bool {
        matches!(
            self,
            Self::Hello { .. } | Self::Status { .. } | Self::Shutdown { .. }
        )
    }

    /// The operation name a refusal is reported against.
    #[must_use]
    pub const fn operation_name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "run.hello",
            Self::Status { .. } => "run.status",
            Self::Shutdown { .. } => "run.shutdown",
            Self::RunStatus { .. } => "run.status",
            Self::Send { .. } => "run.send",
            Self::Submit { .. } => "run.submit",
            Self::ExternalSubmit { .. } => "engagement.assign",
            Self::Wait { .. } => "run.wait",
            Self::Events { .. } => "run.events",
            Self::Respond { .. } => "run.respond",
            Self::Interrupt { .. } => "run.interrupt",
            Self::ExternalInterrupt { .. } => "engagement.cancel",
            Self::Pause { .. } => "run.pause",
            Self::Resume { .. } => "run.resume",
            Self::Reconcile { .. } => "run.reconcile",
            Self::SettleLifecycleTimeout { close: true, .. } => "run.close",
            Self::SettleLifecycleTimeout { close: false, .. } => "run.pause",
            Self::Close { .. } => "run.close",
            Self::ExternalClose { .. } => "engagement.release",
            Self::SetWriterAccess { write: true, .. } => "run.acquire_write",
            Self::SetWriterAccess { write: false, .. } => "run.release_write",
            Self::ExternalSetWriterAccess { write: true, .. } => "engagement.acquire_write",
            Self::ExternalSetWriterAccess { write: false, .. } => "engagement.release_write",
            Self::ResetFence { .. } => "run.controller.reset",
        }
    }

    #[must_use]
    pub const fn external_engagement(&self) -> Option<Uuid> {
        match self {
            Self::ExternalSubmit { engagement_id, .. }
            | Self::ExternalInterrupt { engagement_id, .. }
            | Self::ExternalClose { engagement_id, .. }
            | Self::ExternalSetWriterAccess { engagement_id, .. } => Some(*engagement_id),
            _ => None,
        }
    }
}

/// The Run facts every refusal this worker reports has to name.
///
/// The checked error contract gives each code required `details` members, and
/// most of them describe the Run rather than the fault, so the Run carries
/// them once instead of every refusal site inventing them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunFacts {
    pub run_id: Uuid,
    pub profile: String,
}

impl RunFacts {
    /// The failure context for a fault the Run knows a Thread and Turn for.
    #[must_use]
    pub fn context(
        &self,
        thread_id: Option<String>,
        turn_id: Option<String>,
    ) -> TurnFailureContext {
        TurnFailureContext {
            run_id: self.run_id,
            profile: self.profile.clone(),
            thread_id,
            turn_id,
        }
    }

    /// The failure context for a fault outside any Turn.
    #[must_use]
    pub fn bare(&self) -> TurnFailureContext {
        self.context(None, None)
    }
}

/// This Run's authoritative Controller authority, resolved from durable state
/// on every mutation rather than cached at startup.
///
/// ADR-016 makes the worker the authoritative consumer: a Controller
/// credential the CLI already accepted is revalidated here, against the
/// reset-journal-reconciled binding that is current at effect time, so a
/// credential that was authoritative when the request was composed cannot
/// outlive an operator controller reset that landed in between.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunControllerAuthority {
    state_root: PathBuf,
    run_id: Uuid,
}

impl RunControllerAuthority {
    #[must_use]
    pub const fn new(state_root: PathBuf, run_id: Uuid) -> Self {
        Self { state_root, run_id }
    }

    /// The Run this authority speaks for, so a refusal can name it.
    #[must_use]
    pub const fn run_id(&self) -> Uuid {
        self.run_id
    }

    /// Reread the already-open carrier and authorize it against the Run's
    /// current durable binding.  Callers hold the Run mutation lock across
    /// this call, so the binding cannot advance between the check and the
    /// effect it guards.
    pub fn authorize(
        &self,
        operation: &str,
        carrier: &CredentialCarrier,
    ) -> Result<(), MachineError> {
        let binding = load_reconciled_controller_binding(&self.state_root, self.run_id)?;
        authorize_controller(self.run_id, operation, &binding, carrier).map(|_| ())
    }

    /// Authorize a facade-owned mutation without exposing or deriving the
    /// member Run's Controller secret. This check executes on the drain while
    /// the mutation is serialized, and binds all three durable authorities:
    /// the engagement owner, the Run manifest, and active membership.
    pub fn authorize_external(
        &self,
        engagement_id: Uuid,
        operation: &str,
        carrier: &CredentialCarrier,
    ) -> Result<(), MachineError> {
        let runs = RunStore::new(SystemWorkspacePlatform, &self.state_root);
        let manifest = runs.load_manifest(self.run_id)?;
        let binding = manifest.aggregate_binding.ok_or_else(|| {
            MachineError::new(
                "INTERNAL_ERROR",
                "external engagement integrity check failed",
                false,
                serde_json::json!({"invariant":"specialist Run has no aggregate binding","run_id": self.run_id}),
            )
        })?;
        if binding.aggregate_kind != AggregateKind::ExternalSpecialistEngagement
            || binding.aggregate_id != engagement_id
            || binding.member_kind != AggregateMemberKind::Specialist
        {
            return Err(MachineError::new(
                "INTERNAL_ERROR",
                "external engagement integrity check failed",
                false,
                serde_json::json!({"invariant":"specialist Run aggregate binding does not match the engagement","run_id": self.run_id, "engagement_id": engagement_id}),
            ));
        }
        let database = EngagementStore::workspace_database_path(&self.state_root);
        let store = EngagementStore::open(&database)?;
        store.authorize_external_owner(
            &manifest.workspace_id,
            engagement_id,
            operation,
            carrier,
        )?;
        store.validate_external_member_binding(engagement_id, self.run_id, &binding)?;
        if !store.external_member_is_active(engagement_id, self.run_id)? {
            return Err(MachineError::new(
                "ENGAGEMENT_STATE_CONFLICT",
                "specialist Run is not an active engagement member",
                false,
                serde_json::json!({"run_id": self.run_id, "engagement_id": engagement_id}),
            ));
        }
        Ok(())
    }

    fn authorize_mutation(
        &self,
        operation: &str,
        credential: Option<&CredentialCarrier>,
        external_engagement: Option<Uuid>,
    ) -> Result<(), MachineError> {
        let carrier = credential.ok_or_else(|| {
            MachineError::new(
                "CONTROLLER_MISMATCH",
                format!("controller credential does not authorize {operation}"),
                false,
                serde_json::json!({"run_id":self.run_id,"operation":operation}),
            )
        })?;
        external_engagement.map_or_else(
            || self.authorize(operation, carrier),
            |engagement_id| self.authorize_external(engagement_id, operation, carrier),
        )
    }

    /// Prove that an operator has already fenced this Run with a durable,
    /// unresolved controller reset prepare naming it.
    ///
    /// This is the whole authorization for `ResetFence`.  Writing that prepare
    /// requires the operator capability under `operator.lock` and the Run's
    /// startup lock, so a same-uid caller that has not done it cannot make the
    /// worker answer; and a caller that has done it has already fenced every
    /// mutation, because the same durable record is what
    /// `load_reconciled_controller_binding` fails mutations closed on.
    pub fn reset_fence(&self, confirmation: Uuid, lifecycle: &str) -> Result<(), MachineError> {
        if confirmation != self.run_id {
            return Err(MachineError::invalid_argument(
                "--confirm",
                "confirmation must name this run",
            ));
        }
        if crate::controller::run_reset_prepare_is_pending(&self.state_root, self.run_id)? {
            return Ok(());
        }
        // The checked contract binds `state` to the Run's lifecycle, so it is
        // the one this worker is authoritatively in, not a guess.
        Err(MachineError::new(
            "CONTROLLER_RESET_NOT_ALLOWED",
            "controller reset is not allowed in the current state",
            false,
            serde_json::json!({
                "run_id": self.run_id,
                "state": lifecycle,
                "blockers": ["reset_prepare_absent"],
            }),
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlResponseV1 {
    Hello {
        hello: WorkerHello,
    },
    Status {
        identity: WorkerIdentity,
        lifecycle: String,
        active_turn: Option<String>,
        /// The last Turn this generation drained to a terminal, when the
        /// caller asked with the skew-checked `RunStatus` request.
        ///
        /// It is absent from the wire, not null, whenever there is none.  The
        /// frozen control-v1 `status` answer is exactly its v1 self — this
        /// response type denies unknown fields, so a member a v1 caller has
        /// never heard of is a parse failure for it rather than an extra it
        /// can ignore — and SPEC-007 requires that answer to stay usable
        /// across a Dolgorae binary upgrade in either direction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_terminal: Option<TerminalTurn>,
    },
    /// The frozen control-v1 shutdown answer.
    ///
    /// `terminal_confirmed` is evidence, never optimism: it is true only when
    /// the Run had no Turn in flight or the drain actually observed that
    /// Turn's terminal within the interrupt budget.
    Shutdown {
        identity: WorkerIdentity,
        terminal_confirmed: bool,
    },
    Accepted {
        accepted: AcceptedTurn,
    },
    WaitingInteraction {
        thread_id: String,
        turn_id: String,
        effort: String,
        requests: Vec<Interaction>,
    },
    Terminal {
        terminal: TerminalTurn,
    },
    /// One bounded page of durable events.
    ///
    /// `head_cursor` is the authoritative ledger head this page was read
    /// against.  SPEC-006 has `run events` emit "records through the head
    /// captured at command start", so the caller pages against the head it was
    /// first told rather than chasing a head that keeps moving.
    Events {
        deliveries: Vec<EventDelivery>,
        next_cursor: String,
        head_cursor: String,
    },
    Responded {
        request_id: u64,
        resolution_receipt_id: Option<Uuid>,
    },
    Interrupted {
        thread_id: String,
        turn_id: String,
        effort: String,
    },
    /// The addressed Turn is still running and the caller's own timeout
    /// expired.  SPEC-006: a caller-supplied timeout returns the current
    /// nonterminal state without interrupting the worker.
    Running {
        thread_id: String,
        turn_id: String,
        effort: String,
    },
    /// What the Run authoritatively is, as seen by the thread that performs
    /// every mutation, at a point ordered against all of them.
    ResetFence {
        lifecycle: String,
        thread_id: Option<String>,
        active_turn: Option<String>,
        pending_interactions: usize,
    },
    Closed {
        identity: WorkerIdentity,
        thread_id: Option<String>,
    },
    WriterAccessChanged {
        write: bool,
        writer_generation: u64,
        thread_id: Option<String>,
    },
    /// A refusal the caller reads as a machine error.
    ///
    /// `details` is the code's own required members from the checked error
    /// contract, carried across the socket rather than reconstructed by the
    /// CLI, because only the worker knows which Run fact refused the request.
    Failed {
        code: String,
        message: String,
        retryable: bool,
        details: Value,
    },
    /// Frozen control-v1 identity refusal: byte-identical to the v1 wire, so
    /// it stays a bare code and the CLI supplies the contract details from the
    /// runtime record it addressed.
    Rejected {
        code: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerControlState {
    pub lifecycle: String,
    pub active_turn: Option<String>,
}

#[derive(Clone)]
pub struct WorkerControlServer {
    hello: WorkerHello,
    facts: RunFacts,
    state: Arc<Mutex<WorkerControlState>>,
    stopping: Arc<AtomicBool>,
    run: Arc<Mutex<RunHandles>>,
    authority: Arc<RunControllerAuthority>,
}

impl WorkerControlServer {
    #[must_use]
    pub fn new(
        hello: WorkerHello,
        facts: RunFacts,
        state: WorkerControlState,
        authority: RunControllerAuthority,
    ) -> Self {
        Self {
            hello,
            facts,
            state: Arc::new(Mutex::new(state)),
            stopping: Arc::new(AtomicBool::new(false)),
            run: Arc::new(Mutex::new(RunHandles::default())),
            authority: Arc::new(authority),
        }
    }

    /// Attach the live Run so this control socket serves Turn operations and
    /// observer reads instead of identity alone.  Attachment happens after
    /// replay, while the socket is already answering `status`, so a caller that
    /// arrives during startup is told the truth instead of being refused.
    pub fn attach_run(
        &self,
        session: Arc<WorkerSession>,
        ledger: Arc<Mutex<Ledger>>,
    ) -> Result<(), WorkerProtocolError> {
        // The drain starts before the Run is reachable, so the first caller to
        // arrive finds a Run that is already advancing on its own.
        session.begin(
            self.hello.identity.clone(),
            Arc::clone(&self.state),
            Arc::clone(&self.authority),
        )?;
        let mut run = self.run.lock().map_err(|_| WorkerProtocolError::Io)?;
        run.session = Some(session);
        run.ledger = Some(ledger);
        Ok(())
    }

    /// Attach observation alone, for a worker whose Run owns no app-server
    /// conversation yet.
    pub fn attach_ledger(&self, ledger: Arc<Mutex<Ledger>>) -> Result<(), WorkerProtocolError> {
        self.run.lock().map_err(|_| WorkerProtocolError::Io)?.ledger = Some(ledger);
        Ok(())
    }

    /// Release the Run before the worker exits so the durable ledger closes
    /// while this process still owns its startup slot.
    pub fn detach_run(&self) -> Result<(), WorkerProtocolError> {
        let handles = std::mem::take(&mut *self.run.lock().map_err(|_| WorkerProtocolError::Io)?);
        // Every control caller has been joined by now, so this is the last
        // reference: stopping here keeps the drain from outliving the socket
        // it was serving.
        if let Some(session) = handles.session {
            session.shutdown();
        }
        Ok(())
    }

    pub fn replace_state(&self, state: WorkerControlState) -> Result<(), WorkerProtocolError> {
        *self.state.lock().map_err(|_| WorkerProtocolError::Io)? = state;
        Ok(())
    }

    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    /// SIGTERM during an active Turn.
    ///
    /// docs/specs/README.md: the worker "sends `turn/interrupt`, waits up to five seconds
    /// for a terminal event, fsyncs terminal evidence when observed, and
    /// records `outcome_unknown` on expiry before generation cleanup".  That
    /// is the same drain-ordered sequence an identity-verified `shutdown`
    /// performs, so it is the same routine; the signal thread simply does not
    /// have a caller to answer.
    pub fn terminate(&self) -> Result<(), WorkerProtocolError> {
        let session = self
            .run
            .lock()
            .map_err(|_| WorkerProtocolError::Io)?
            .session()
            .map(Arc::clone);
        if let Some(session) = session {
            let _ = session.settle_before_shutdown();
        }
        self.stop();
        Ok(())
    }

    /// Serve independent bounded connections until an identity-authorized
    /// shutdown request is accepted. Each caller owns a thread, so an observer
    /// that stops reading cannot block unrelated mutation or event draining.
    pub fn serve(self, lease: &WorkerSocketLease) -> Result<(), WorkerProtocolError> {
        self.hello.identity.validate()?;
        lease
            .listener
            .set_nonblocking(true)
            .map_err(|_| WorkerProtocolError::Io)?;
        let stopping = Arc::clone(&self.stopping);
        let mut callers = Vec::new();
        while !stopping.load(Ordering::Acquire) {
            match lease.listener.accept() {
                Ok((stream, _)) => {
                    // Darwin propagates the listener's non-blocking flag to
                    // every accepted socket, which would make the per-caller
                    // read and write timeouts inert and drop a caller that had
                    // not managed to write yet. Each caller owns a thread, so
                    // it can afford to block within its own bounded budget.
                    stream
                        .set_nonblocking(false)
                        .map_err(|_| WorkerProtocolError::Io)?;
                    let hello = self.hello.clone();
                    let facts = self.facts.clone();
                    let state = Arc::clone(&self.state);
                    let stopping = Arc::clone(&stopping);
                    let authority = Arc::clone(&self.authority);
                    let run = self
                        .run
                        .lock()
                        .map_err(|_| WorkerProtocolError::Io)?
                        .clone();
                    callers.push(thread::spawn(move || {
                        let _ = serve_control_caller(
                            stream, &hello, &facts, &state, &stopping, &run, &authority,
                        );
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                // A caller that hangs up between connect and accept, or a
                // signal that interrupts the accept, is that caller's problem
                // alone. Neither may take the Run's whole control plane down.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(WorkerProtocolError::Io),
            }
        }
        for caller in callers {
            caller.join().map_err(|_| WorkerProtocolError::Io)?;
        }
        Ok(())
    }
}

/// Everything the hidden worker needs to own this Run's app-server connection.
///
/// It is deliberately part of the bootstrap rather than discovered at runtime:
/// the worker must never widen its own authority by re-deriving which server,
/// account home, or model it is allowed to speak to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerSessionBootstrap {
    pub app_server_socket: PathBuf,
    pub canonical_codex_home: String,
    pub server_key: String,
    pub server_epoch: u64,
    pub controller_id: Uuid,
    pub control_mode: String,
    pub fixed_model: String,
    pub default_effort: String,
    pub supported_efforts: Vec<String>,
    pub cwd: PathBuf,
    pub developer_instructions: String,
    pub sandbox: String,
    pub approval_policy: String,
    #[serde(default)]
    pub safety_policy: SessionSafetyPolicy,
    pub artifact_root: PathBuf,
    pub attach: SessionAttach,
    pub transport_timeout_seconds: u64,
    #[serde(default)]
    pub dedicated_server: Option<DedicatedServerBootstrap>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DedicatedServerBootstrap {
    pub socket_path: PathBuf,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub log_path: PathBuf,
    pub executable_device: u64,
    pub executable_inode: u64,
    pub executable_sha256: String,
    pub server_epoch: u64,
}

impl DedicatedServerBootstrap {
    fn validate(&self) -> Result<(), WorkerProtocolError> {
        if !self.socket_path.is_absolute()
            || self.argv.first().is_none_or(String::is_empty)
            || !self.cwd.is_absolute()
            || !self.log_path.is_absolute()
            || self.executable_device == 0
            || self.executable_inode == 0
            || decode_sha256(&self.executable_sha256).is_none()
            || self.server_epoch == 0
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "attach", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionAttach {
    Start,
    Resume {
        thread_id: String,
    },
    Fork {
        source_thread_id: String,
        last_turn_id: String,
    },
}

impl WorkerSessionBootstrap {
    pub fn validate(&self) -> Result<(), WorkerProtocolError> {
        if !self.app_server_socket.is_absolute()
            || !Path::new(&self.canonical_codex_home).is_absolute()
            || decode_sha256(&self.server_key).is_none()
            || self.server_epoch == 0
            || self.controller_id.get_version_num() != 7
            || !matches!(
                self.control_mode.as_str(),
                "direct_interactive" | "managed_agent"
            )
            || self.fixed_model.is_empty()
            || self.default_effort.is_empty()
            || !self.supported_efforts.contains(&self.default_effort)
            || !self.cwd.is_absolute()
            || self.sandbox.is_empty()
            || self.approval_policy.is_empty()
            || !self.artifact_root.is_absolute()
            || self.transport_timeout_seconds == 0
            || self.transport_timeout_seconds > 86_400
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        if self.safety_policy == SessionSafetyPolicy::ReviewerReadOnly
            && (self.sandbox != "read-only" || self.approval_policy != "never")
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        if let Some(dedicated) = &self.dedicated_server {
            dedicated.validate()?;
            if dedicated.socket_path != self.app_server_socket
                || dedicated.server_epoch != self.server_epoch
            {
                return Err(WorkerProtocolError::InvalidRuntimeRecord);
            }
        }
        Ok(())
    }
}

/// Durable store for a final response too large to carry inline.
///
/// Content addressing keeps a replayed Turn from minting a second copy of a
/// response the Run has already published.
#[derive(Clone, Debug)]
pub struct FileArtifactStore {
    root: PathBuf,
}

impl FileArtifactStore {
    pub fn open(root: &Path, uid: u32) -> Result<Self, WorkerProtocolError> {
        if !root.is_absolute() {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        if fs::symlink_metadata(root).is_err() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root)
                .map_err(|_| WorkerProtocolError::Io)?;
        }
        let metadata = fs::symlink_metadata(root).map_err(|_| WorkerProtocolError::Io)?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        Ok(Self {
            root: root.to_owned(),
        })
    }
}

impl ResponseArtifactStore for FileArtifactStore {
    fn store(&mut self, bytes: &[u8]) -> Result<StoredArtifact, TurnError> {
        // Named by its own UUIDv7 rather than by content, because the artifact
        // contract identifies an artifact by identity and creation instant; a
        // content digest cannot carry either.
        let artifact_id = Uuid::now_v7();
        let path = self.root.join(format!("{artifact_id}.bin"));
        let temporary = self.root.join(format!(".{artifact_id}"));
        let write = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        })();
        if let Err(error) = write {
            let _ = fs::remove_file(&temporary);
            return Err(TurnError::Artifact(error.to_string()));
        }
        Ok(StoredArtifact {
            artifact_id,
            created_at: SystemLedgerClock::default().timestamp(),
        })
    }
}

type SessionCoordinator = TurnCoordinator<
    SessionServer,
    SharedLedgerJournal<SystemLedgerClock, NoFaults>,
    FileArtifactStore,
>;

/// How many terminal Turns a Run remembers.
///
/// A caller that reconnects after its Send was cut off, or one whose Send was
/// overtaken by the Turn after it, still has to find its own outcome rather
/// than the newest one.
pub const REMEMBERED_TERMINAL_TURNS: usize = 64;

/// One mutation a control caller handed to the Run's drain thread.
enum SessionCommand {
    Accept {
        request: TurnControlRequest,
        delivery: DeliveryMode,
    },
    Respond {
        request_id: u64,
        idempotency_key: String,
        response: Value,
    },
    Interrupt,
    Pause {
        interrupt: bool,
    },
    Resume,
    Reconcile,
    SettleLifecycleTimeout {
        close: bool,
    },
    Close {
        interrupt: bool,
    },
    SetWriterAccess {
        write: bool,
        writer_generation: u64,
        transaction_id: Uuid,
    },
    /// Report the Run's authoritative state to a fenced controller reset.
    ResetFence {
        confirmation: Uuid,
    },
    /// Interrupt the active Turn on behalf of shutdown.
    ///
    /// Its authority is the frozen control-v1 identity check (or this
    /// process's own SIGTERM), not a Controller credential, so it is answered
    /// beside `ResetFence` — before any Controller revalidation — while still
    /// travelling the drain queue so it is ordered against every mutation.
    ShutdownInterrupt,
    /// Give up the active Turn's outcome durably, after the shutdown
    /// interrupt's terminal wait expired.
    ShutdownAbandon,
}

impl SessionCommand {
    /// Whether this is shutdown's own work rather than a caller's.
    ///
    /// Shutdown fences the Run against new mutations and then performs two of
    /// its own, so the fence has to be able to tell them apart; and neither of
    /// them is refused for a transport this Run has already lost, because
    /// recording the loss durably is exactly what the second one is for.
    const fn shutdown_owned(&self) -> bool {
        matches!(self, Self::ShutdownInterrupt | Self::ShutdownAbandon)
    }
}

/// A queued mutation together with the Controller credential the drain
/// revalidates immediately before the effect it authorizes.
///
/// The credential travels with the work rather than being checked where the
/// work was accepted: ADR-016 wants the authoritative check on the thread that
/// performs the effect, at the moment it performs it.
struct SessionMutation {
    operation: &'static str,
    /// Absent only for work whose authority is not a Controller credential:
    /// today that is the operator's fenced controller reset.
    credential: Option<CredentialCarrier>,
    /// Present only for facade-private aggregate-owner delegation.
    external_engagement: Option<Uuid>,
    command: SessionCommand,
    outcome: Arc<MutationOutcome>,
}

/// Where a queued mutation's answer is left for the caller that queued it.
#[derive(Default)]
struct MutationOutcome {
    state: Mutex<MutationOutcomeState>,
    ready: Condvar,
}

#[derive(Default)]
struct MutationOutcomeState {
    started: bool,
    settled: Option<ControlResponseV1>,
}

impl MutationOutcome {
    fn start(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.started = true;
        }
        self.ready.notify_all();
    }

    /// Wait until the drain begins this exact mutation.  Shutdown uses this
    /// edge as the start of its interrupt/terminal budget; queue time behind a
    /// prior mutation is not time the interrupted Turn was allowed to settle.
    fn await_started(&self, budget: Duration) -> bool {
        let Some(deadline) = Instant::now().checked_add(budget) else {
            return false;
        };
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        loop {
            if state.started {
                return true;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let Ok((guard, timeout)) = self.ready.wait_timeout(state, remaining) else {
                return false;
            };
            state = guard;
            if timeout.timed_out() && !state.started {
                return false;
            }
        }
    }

    fn settle(&self, response: ControlResponseV1) {
        if let Ok(mut state) = self.state.lock() {
            state.settled = Some(response);
        }
        self.ready.notify_all();
    }

    /// Wait for the drain to answer, bounded by the control call budget so a
    /// worker thread never outlives the caller waiting on it.
    fn await_settled(&self, budget: Duration) -> Option<ControlResponseV1> {
        let deadline = std::time::Instant::now().checked_add(budget)?;
        let mut state = self.state.lock().ok()?;
        loop {
            if let Some(response) = state.settled.take() {
                return Some(response);
            }
            let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
            let (guard, timeout) = self.ready.wait_timeout(state, remaining).ok()?;
            state = guard;
            if timeout.timed_out() && state.settled.is_none() {
                return None;
            }
        }
    }
}

/// What the Run's drain thread does next.
enum SessionWork {
    Message(Result<Value, TurnError>),
    Mutation(SessionMutation),
    Stop,
}

/// The drain thread's inbox: app-server messages the reader has read, and
/// mutations control callers have queued.
///
/// The reader reads only on an explicit grant, so the drain always knows
/// whether a read is outstanding.  That is what lets the drain take the socket
/// over for a solicited streamed read without racing the reader for the next
/// frame, and it is why a message the drain has to carry over is always
/// answered before a new grant is issued.
struct SessionMailbox {
    facts: RunFacts,
    state: Mutex<MailboxState>,
    changed: Condvar,
}

#[derive(Default)]
struct MailboxState {
    /// A message the reader has read and the drain has not taken yet.
    delivered: Option<Result<Value, TurnError>>,
    /// Messages read out of turn while a request awaited its own reply.
    carried: VecDeque<Value>,
    /// True while the reader holds a grant it has not spent.
    reading: bool,
    /// The transport failure that ended the reader, latched: once the reader
    /// is gone no grant will ever be answered again, so every later read has
    /// to report the same loss rather than wait for one.
    lost: Option<TurnError>,
    mutations: VecDeque<SessionMutation>,
    /// True once shutdown has claimed this Run.  Work already queued still
    /// drains — it was accepted before shutdown began and is ordered ahead of
    /// it — but nothing new is taken on, so shutdown's interrupt and its
    /// terminal wait cannot be overtaken by a Turn that started behind them.
    fenced: bool,
    stopping: bool,
}

impl SessionMailbox {
    fn new(facts: RunFacts) -> Self {
        Self {
            facts,
            state: Mutex::new(MailboxState::default()),
            changed: Condvar::new(),
        }
    }

    fn lost() -> TurnError {
        TurnError::transport(TransportStage::Shutdown, "app-server drain is stopping")
    }

    /// Reader: block until the drain grants one read.  `false` means stop.
    fn await_grant(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        loop {
            if state.stopping {
                return false;
            }
            if state.reading {
                return true;
            }
            let Ok(guard) = self.changed.wait(state) else {
                return false;
            };
            state = guard;
        }
    }

    /// Reader: hand over one message and give the grant back.
    fn deliver(&self, message: Result<Value, TurnError>) {
        if let Ok(mut state) = self.state.lock() {
            if let Err(error) = &message {
                state.lost = Some(error.clone());
            }
            state.delivered = Some(message);
            state.reading = false;
        }
        self.changed.notify_all();
    }

    /// Drain: keep one message that arrived before the reply it was read for.
    fn carry(&self, value: Value) -> Result<(), TurnError> {
        let mut state = self.state.lock().map_err(|_| Self::lost())?;
        if state.carried.len() >= MAX_CARRIED_MESSAGES {
            return Err(TurnError::CorrelationMismatch);
        }
        state.carried.push_back(value);
        Ok(())
    }

    /// Drain: the next app-server message read fresh off the socket, bounded
    /// by `deadline`.
    ///
    /// The bound is what a request needs and the drain's idle wait must not
    /// have: an app-server that never answers a request has failed, while one
    /// that simply has nothing to say has not.
    ///
    /// Carried messages are deliberately not offered here: they are already
    /// classified as notifications, and a request is only ever looking for its
    /// own reply.
    fn next_fresh(&self, deadline: Instant) -> Result<Value, TurnError> {
        let mut state = self.state.lock().map_err(|_| Self::lost())?;
        loop {
            if let Some(message) = state.delivered.take() {
                self.changed.notify_all();
                return message;
            }
            if state.stopping {
                return Err(Self::lost());
            }
            if let Some(error) = state.lost.clone() {
                return Err(error);
            }
            if !state.reading {
                state.reading = true;
                self.changed.notify_all();
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(TurnError::transport(
                    TransportStage::Read,
                    "app-server did not answer within the transport budget",
                ));
            };
            state = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| Self::lost())?
                .0;
        }
    }

    /// Drain: the next app-server message, carried ones first.
    fn next_message(&self, deadline: Instant) -> Result<Value, TurnError> {
        if let Some(carried) = self
            .state
            .lock()
            .map_err(|_| Self::lost())?
            .carried
            .pop_front()
        {
            return Ok(carried);
        }
        self.next_fresh(deadline)
    }

    /// Drain: the next unit of work.
    ///
    /// Carried messages come first so the drain has folded them in before it
    /// ever grants another read; that ordering is what keeps a read from being
    /// outstanding while the drain still owes the Run a message.
    fn next_work(&self) -> SessionWork {
        let Ok(mut state) = self.state.lock() else {
            return SessionWork::Stop;
        };
        loop {
            if let Some(carried) = state.carried.pop_front() {
                return SessionWork::Message(Ok(carried));
            }
            if let Some(mutation) = state.mutations.pop_front() {
                return SessionWork::Mutation(mutation);
            }
            if let Some(message) = state.delivered.take() {
                self.changed.notify_all();
                return SessionWork::Message(message);
            }
            if state.stopping {
                return SessionWork::Stop;
            }
            if let Some(error) = state.lost.clone() {
                return SessionWork::Message(Err(error));
            }
            if !state.reading {
                state.reading = true;
                self.changed.notify_all();
            }
            let Ok(guard) = self.changed.wait(state) else {
                return SessionWork::Stop;
            };
            state = guard;
        }
    }

    /// Caller: queue one mutation for the drain, or say why it was refused.
    fn submit(&self, mutation: SessionMutation) -> Option<ControlResponseV1> {
        let stopping = TurnError::transport(TransportStage::Shutdown, "run is stopping");
        let shutdown_owned = mutation.command.shutdown_owned();
        let Ok(mut state) = self.state.lock() else {
            return Some(failed(&stopping, &self.facts.bare()));
        };
        if state.stopping {
            return Some(failed(&stopping, &self.facts.bare()));
        }
        // Shutdown's own work is never refused here.  The fence is what it
        // raised, and a lost transport is the very condition its durable
        // `outcome_unknown` record exists to answer — refusing it would drop
        // the record precisely when the Run needs it most.
        if !shutdown_owned && (state.fenced || state.lost.is_some()) {
            return Some(failed(&stopping, &self.facts.bare()));
        }
        if state.mutations.len() >= MAX_QUEUED_MUTATIONS {
            return Some(run_busy(
                &self.facts,
                "turn",
                "run has too many mutations waiting",
            ));
        }
        state.mutations.push_back(mutation);
        self.changed.notify_all();
        None
    }

    /// Claim this Run for shutdown: no new caller mutation is taken on.
    ///
    /// This is raised before shutdown reads which Turn is live, so the Turn it
    /// interrupts and waits for is the last one this generation can have.
    /// Without it a Turn accepted between the interrupt and the terminal wait
    /// would be torn down with no interrupt sent and no durable record of its
    /// outcome, which is the one thing shutdown exists to prevent.
    fn fence(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.fenced = true;
        }
        self.changed.notify_all();
    }

    /// Stop the drain and refuse every mutation still queued.
    fn stop(&self) -> Vec<SessionMutation> {
        let refused = match self.state.lock() {
            Ok(mut state) => {
                state.stopping = true;
                state.mutations.drain(..).collect()
            }
            Err(_) => Vec::new(),
        };
        self.changed.notify_all();
        refused
    }
}

/// What the Run publishes after every step its drain takes, so a caller waits
/// on Run state instead of on the Run itself.
struct SessionProgress {
    facts: RunFacts,
    state: Mutex<ProgressState>,
    changed: Condvar,
}

struct ProgressState {
    control: WorkerControlState,
    thread_id: Option<String>,
    /// The effort the active Turn was started with, so a nonterminal answer
    /// reports what that Turn is actually running at.
    active_effort: Option<String>,
    pending: Vec<Interaction>,
    terminals: VecDeque<TerminalTurn>,
    /// Turns this generation accepted and then gave up the outcome of.
    ///
    /// A Turn whose outcome was lost is the opposite of a Turn this Run never
    /// had, and a bounded terminal memory alone cannot tell them apart: both
    /// are simply absent from it.  Remembering them is what lets a caller that
    /// comes back be told the uncertainty the ledger already holds instead of
    /// `TURN_NOT_FOUND`.
    abandoned: VecDeque<String>,
    last_terminal: Option<TerminalTurn>,
    fatal: Option<TurnError>,
    closed: bool,
    stopped: bool,
}

impl SessionProgress {
    fn new(facts: RunFacts) -> Self {
        Self {
            facts,
            state: Mutex::new(ProgressState {
                control: WorkerControlState {
                    lifecycle: "idle".to_owned(),
                    active_turn: None,
                },
                thread_id: None,
                active_effort: None,
                pending: Vec::new(),
                terminals: VecDeque::new(),
                abandoned: VecDeque::new(),
                last_terminal: None,
                fatal: None,
                closed: false,
                stopped: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn publish(
        &self,
        control: WorkerControlState,
        thread_id: Option<String>,
        active_effort: Option<String>,
        pending: Vec<Interaction>,
    ) {
        if let Ok(mut state) = self.state.lock() {
            state.control = control;
            state.thread_id = thread_id;
            state.active_effort = active_effort;
            state.pending = pending;
        }
        self.changed.notify_all();
    }

    /// Remember one Turn's terminal, bounded, so every caller that was waiting
    /// on that Turn finds the same single outcome.
    fn settle(&self, terminal: TerminalTurn) {
        if let Ok(mut state) = self.state.lock() {
            if !state
                .terminals
                .iter()
                .any(|known| known.turn_id == terminal.turn_id)
            {
                if state.terminals.len() >= REMEMBERED_TERMINAL_TURNS {
                    state.terminals.pop_front();
                }
                state.terminals.push_back(terminal.clone());
            }
            state.last_terminal = Some(terminal);
        }
        self.changed.notify_all();
    }

    fn fail(&self, error: TurnError) {
        if let Ok(mut state) = self.state.lock() {
            state.fatal = Some(error);
        }
        self.changed.notify_all();
    }

    /// Remember one Turn this Run gave up, bounded like its terminals.
    fn abandon(&self, turn_id: String) {
        if let Ok(mut state) = self.state.lock() {
            if state.abandoned.iter().any(|known| *known == turn_id) {
                return;
            }
            if state.abandoned.len() >= REMEMBERED_TERMINAL_TURNS {
                state.abandoned.pop_front();
            }
            state.abandoned.push_back(turn_id);
        }
        self.changed.notify_all();
    }

    fn mark_closed(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
        self.changed.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.closed)
    }

    /// Whether this Run has already given the named Turn up.
    fn gave_up(&self, turn_id: &str) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.abandoned.iter().any(|known| known == turn_id))
    }

    fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.stopped = true;
        }
        self.changed.notify_all();
    }

    fn control_state(&self) -> WorkerControlState {
        match self.state.lock() {
            Ok(state) => state.control.clone(),
            Err(_) => WorkerControlState {
                lifecycle: "outcome_unknown".to_owned(),
                active_turn: None,
            },
        }
    }

    /// The last Turn this generation drained to a terminal, as observed.
    fn last_terminal(&self) -> Option<TerminalTurn> {
        self.state.lock().ok()?.last_terminal.clone()
    }

    fn thread_id(&self) -> Option<String> {
        self.state.lock().ok()?.thread_id.clone()
    }

    /// Block until `turn_id` reaches a terminal or opens an interaction.
    ///
    /// The wait is on published Run state, never on the Run's own lock, so a
    /// caller blocked here cannot keep an interrupt or an interaction answer
    /// from being delivered.  It is bounded by the control call budget: past
    /// that a caller reconnects and rejoins rather than holding a worker
    /// thread open, and the Turn keeps running either way.
    fn await_turn(&self, turn_id: &str, caller_budget: Option<Duration>) -> ControlResponseV1 {
        let context = self
            .facts
            .context(self.thread_id(), Some(turn_id.to_owned()));
        // A caller's own timeout is honoured as written, up to the ceiling a
        // held worker thread has to have.  Quietly shortening it would report
        // "still running" at a moment the caller never asked about, and a
        // caller that named no timeout asked to wait for the Turn rather than
        // for a fixed slice of it.
        let budget = caller_budget.map_or(MAX_TURN_WAIT_TIMEOUT, |caller| {
            caller.min(MAX_TURN_WAIT_TIMEOUT)
        });
        let Some(deadline) = std::time::Instant::now().checked_add(budget) else {
            return failed(
                &TurnError::Journal("run clock is unusable".to_owned()),
                &context,
            );
        };
        let Ok(mut state) = self.state.lock() else {
            return failed(
                &TurnError::Journal("run progress is poisoned".to_owned()),
                &context,
            );
        };
        loop {
            if let Some(terminal) = state
                .terminals
                .iter()
                .find(|known| known.turn_id == turn_id)
                .cloned()
            {
                return terminal_response(terminal);
            }
            // A Turn this Run gave up has an answer already, and it is the
            // same one for every caller waiting on it.  Waiting longer cannot
            // improve it: nothing will ever observe that Turn again.
            if state.abandoned.iter().any(|known| known == turn_id) {
                return failed(&TurnError::OutcomeUnknown, &context);
            }
            if state.control.active_turn.as_deref() == Some(turn_id) && !state.pending.is_empty() {
                return ControlResponseV1::WaitingInteraction {
                    thread_id: state.thread_id.clone().unwrap_or_default(),
                    turn_id: turn_id.to_owned(),
                    effort: state.active_effort.clone().unwrap_or_default(),
                    requests: state.pending.clone(),
                };
            }
            if let Some(error) = state.fatal.clone() {
                return failed(&error, &context);
            }
            if state.stopped {
                return failed(
                    &TurnError::transport(TransportStage::Shutdown, "run is stopping"),
                    &context,
                );
            }
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                // SPEC-006: a caller-supplied timeout returns the current
                // nonterminal state without interrupting the worker, and uses
                // exit 0.  Without one the caller waited out the whole ceiling
                // a held worker thread has, and is told to rejoin.
                if caller_budget.is_none() {
                    return run_busy(
                        &self.facts,
                        "turn",
                        "turn is still running; rejoin it with run wait",
                    );
                }
                return ControlResponseV1::Running {
                    thread_id: state.thread_id.clone().unwrap_or_default(),
                    turn_id: turn_id.to_owned(),
                    effort: state.active_effort.clone().unwrap_or_default(),
                };
            };
            let Ok((guard, _)) = self.changed.wait_timeout(state, remaining) else {
                return failed(
                    &TurnError::Journal("run progress is poisoned".to_owned()),
                    &context,
                );
            };
            state = guard;
        }
    }

    /// Wait for one Turn's terminal and nothing else, for shutdown.
    ///
    /// Unlike the Wait verb this does not settle on an interaction pause: the
    /// interrupt has already been sent, so a Turn that pauses on its way out
    /// is still worth the rest of the budget.  Only an observed terminal
    /// answers true; everything else — a lost transport, a stopped Run, the
    /// budget expiring — is evidence the outcome was not observed.
    fn await_terminal_evidence(&self, turn_id: &str, budget: Duration) -> bool {
        let Some(deadline) = Instant::now().checked_add(budget) else {
            return false;
        };
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        loop {
            if state.terminals.iter().any(|known| known.turn_id == turn_id) {
                return true;
            }
            if state.fatal.is_some() || state.stopped {
                return false;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let Ok((guard, _)) = self.changed.wait_timeout(state, remaining) else {
                return false;
            };
            state = guard;
        }
    }

    /// The Wait verb: rejoin the Turn the caller named.
    ///
    /// SPEC-006 has `wait` require both run and turn IDs, so the answer is
    /// always about the addressed Turn: a Turn that already settled returns
    /// its own remembered outcome, the live Turn is waited on, and a Turn this
    /// Run never had is `TURN_NOT_FOUND` rather than whatever is running now.
    fn await_named(&self, turn_id: &str, caller_budget: Option<Duration>) -> ControlResponseV1 {
        let context = self
            .facts
            .context(self.thread_id(), Some(turn_id.to_owned()));
        let known = match self.state.lock() {
            Ok(state) => {
                state.terminals.iter().any(|known| known.turn_id == turn_id)
                    || state.abandoned.iter().any(|known| known == turn_id)
                    || state.control.active_turn.as_deref() == Some(turn_id)
            }
            Err(_) => {
                return failed(
                    &TurnError::Journal("run progress is poisoned".to_owned()),
                    &context,
                );
            }
        };
        if !known {
            return turn_not_found(&self.facts, turn_id);
        }
        self.await_turn(turn_id, caller_budget)
    }
}

/// The registered refusal for a Turn this Run has no record of.
///
/// "No record" is this generation's own bounded memory: the live Turn plus the
/// last `REMEMBERED_TERMINAL_TURNS` terminals.  Reading a Turn older than that
/// out of the durable ledger belongs with the restart/replay path that has to
/// rebuild the same memory, and is not decided here.
fn turn_not_found(facts: &RunFacts, turn_id: &str) -> ControlResponseV1 {
    ControlResponseV1::Failed {
        code: "TURN_NOT_FOUND".to_owned(),
        message: "the addressed turn is absent from this run".to_owned(),
        retryable: false,
        details: serde_json::json!({"run_id": facts.run_id, "turn_id": turn_id}),
    }
}

/// The coordinator's view of the app-server: every send goes straight to the
/// shared write half, and every read comes from the drain's mailbox.
///
/// Sending never waits on the reader, which is what lets an interrupt or an
/// interaction answer reach the app-server while a Turn is still running.
struct SessionServer {
    wire: Arc<DuplexConnection>,
    mailbox: Arc<SessionMailbox>,
    /// How long one request may go unanswered before the app-server has
    /// failed rather than merely been slow.
    budget: Duration,
    next_request_id: u64,
}

impl SessionServer {
    fn new(wire: Arc<DuplexConnection>, mailbox: Arc<SessionMailbox>, budget: Duration) -> Self {
        Self {
            wire,
            mailbox,
            budget,
            next_request_id: 1,
        }
    }

    fn deadline(&self) -> Instant {
        Instant::now()
            .checked_add(self.budget)
            .unwrap_or_else(Instant::now)
    }

    fn allocate(&mut self) -> Result<u64, TurnError> {
        let id = self.next_request_id;
        self.next_request_id = id.checked_add(1).ok_or(TurnError::CorrelationMismatch)?;
        Ok(id)
    }

    fn send(&self, value: &Value) -> Result<(), TurnError> {
        self.wire.send(value).map_err(TurnError::from)
    }
}

impl AppServer for SessionServer {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, TurnError> {
        let id = self.allocate()?;
        self.send(&serde_json::json!({"id": id, "method": method, "params": params}))?;
        let deadline = self.deadline();
        for _ in 0..MAX_CARRIED_MESSAGES {
            let value = self.mailbox.next_fresh(deadline)?;
            if value.get("method").and_then(Value::as_str).is_some() {
                self.mailbox.carry(value)?;
                continue;
            }
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                return Err(TurnError::CorrelationMismatch);
            }
            // The app-server answered this very request and refused it.  That
            // is a decision, not a lost answer: reporting it as a transport
            // failure would quarantine a Run whose state nothing is uncertain
            // about.
            if let Some(error) = value.get("error") {
                return Err(TurnError::rejected(error));
            }
            return value
                .get("result")
                .cloned()
                .ok_or(TurnError::CorrelationMismatch);
        }
        Err(TurnError::CorrelationMismatch)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), TurnError> {
        self.send(&serde_json::json!({"method": method, "params": params}))
    }

    fn next_message(&mut self) -> Result<Value, TurnError> {
        let value = self.mailbox.next_message(self.deadline())?;
        if value.get("method").and_then(Value::as_str).is_none() {
            return Err(TurnError::CorrelationMismatch);
        }
        Ok(value)
    }

    fn respond_result(&mut self, id: u64, result: Value) -> Result<(), TurnError> {
        self.send(&serde_json::json!({"id": id, "result": result}))
    }

    fn respond_sensitive_result(&mut self, id: u64, result: &mut Value) -> Result<(), TurnError> {
        let mut envelope = serde_json::json!({"id": id, "result": result.take()});
        let outcome = self.wire.send_sensitive(&envelope).map_err(TurnError::from);
        crate::turn::zeroize_protected_json(&mut envelope);
        outcome
    }

    fn respond_error(&mut self, id: u64, code: i64, message: &str) -> Result<(), TurnError> {
        self.send(&serde_json::json!({"id": id, "error": {"code": code, "message": message}}))
    }

    fn request_streamed(
        &mut self,
        method: &str,
        params: Value,
        sink: &mut dyn SolicitedSink,
    ) -> Result<(), TurnError> {
        let id = self.allocate()?;
        self.send(&serde_json::json!({"id": id, "method": method, "params": params}))?;
        // Only the drain reaches here, and only while folding in a message it
        // has already taken, so no read grant is outstanding and taking the
        // socket over cannot race the reader for the next frame.
        let outcome = self.wire.receive_solicited(id, &mut StreamedSink(sink));
        while let Some(queued) = self.wire.take_queued() {
            self.mailbox.carry(queued)?;
        }
        outcome.map(|_| ()).map_err(TurnError::from)
    }
}

/// Adapts the coordinator's trait-object sink back to the generic one the
/// transport streams into.
struct StreamedSink<'a>(&'a mut dyn SolicitedSink);

impl SolicitedSink for StreamedSink<'_> {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError> {
        self.0.accept(chunk)
    }
}

/// The Run's live app-server conversation, owned by the worker process.
///
/// The worker drains the app-server continuously on its own thread for as long
/// as the Run exists, so an accepted Turn advances whether or not any caller is
/// waiting on it.  Callers never hold the Run still: a mutation is queued for
/// the drain, which revalidates its Controller credential and performs it, and
/// an outcome is read from published Run state.
pub struct WorkerSession {
    facts: RunFacts,
    wire: Arc<DuplexConnection>,
    mailbox: Arc<SessionMailbox>,
    progress: Arc<SessionProgress>,
    /// The coordinator, until the drain thread takes ownership of it.
    coordinator: Mutex<Option<SessionCoordinator>>,
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
    dedicated_server: Mutex<Option<OwnedDedicatedServer>>,
}

struct OwnedDedicatedServer {
    child: Child,
    identity: DedicatedServerIdentity,
    socket_path: PathBuf,
    socket_identity: SocketIdentity,
}

impl OwnedDedicatedServer {
    fn identity(&self) -> &DedicatedServerIdentity {
        &self.identity
    }
}

/// One Turn as a control caller asks for it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnControlRequest {
    pub message: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub images: Vec<TurnControlImage>,
}

/// One `--image` value as the caller wrote it.
///
/// The detail token travels with the path because docs/specs/README.md stores "the
/// canonical path, detail, byte length, and streaming SHA-256" and makes the
/// tuple part of idempotency normalization: dropping the token here would
/// silently downgrade every image to `auto` and make two different requests
/// share one key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnControlImage {
    pub detail: ImageDetail,
    pub path: PathBuf,
}

impl WorkerSession {
    pub fn connect(
        session: &WorkerSessionBootstrap,
        facts: RunFacts,
        ledger: Arc<Mutex<Ledger>>,
        run_generation: u64,
        uid: u32,
        state_root: &Path,
    ) -> Result<Self, WorkerProtocolError> {
        session.validate()?;
        let foreign_diagnostics = Box::new(ProfileForeignDiagnostics {
            profile_root: profile_diagnostics_root(state_root, &session.server_key)?,
        });
        let artifacts = FileArtifactStore::open(&session.artifact_root, uid)?;
        let mut dedicated_server = session
            .dedicated_server
            .as_ref()
            .map(start_dedicated_server)
            .transpose()?;
        let wire = match DuplexConnection::connect(
            &session.app_server_socket,
            Duration::from_secs(session.transport_timeout_seconds),
        ) {
            Ok(wire) => Arc::new(wire),
            Err(_) => {
                let _ = stop_dedicated_server(&mut dedicated_server);
                return Err(WorkerProtocolError::AppServerUnavailable);
            }
        };
        let attach = match &session.attach {
            SessionAttach::Start => ThreadAttach::Start,
            SessionAttach::Resume { thread_id } => ThreadAttach::Resume {
                thread_id: thread_id.clone(),
            },
            SessionAttach::Fork {
                source_thread_id,
                last_turn_id,
            } => ThreadAttach::Fork {
                source_thread_id: source_thread_id.clone(),
                last_turn_id: last_turn_id.clone(),
            },
        };
        let mailbox = Arc::new(SessionMailbox::new(facts.clone()));
        // The reader starts before the handshake so `initialize` correlates its
        // own reply through the same mailbox every later request uses.
        let reader = thread::spawn({
            let wire = Arc::clone(&wire);
            let mailbox = Arc::clone(&mailbox);
            move || read_app_server(&wire, &mailbox)
        });
        let (replayed, interaction_decisions) = {
            let ledger = ledger
                .lock()
                .map_err(|_| WorkerProtocolError::LedgerReplay)?;
            let replayed = ledger
                .projection()
                .map_err(|_| WorkerProtocolError::LedgerReplay)?;
            let interaction_decisions = ledger
                .approval_decisions()
                .map_err(|_| WorkerProtocolError::LedgerReplay)?;
            (replayed, interaction_decisions)
        };
        let coordinator = TurnCoordinator::initialize(
            SessionServer::new(
                Arc::clone(&wire),
                Arc::clone(&mailbox),
                Duration::from_secs(session.transport_timeout_seconds),
            ),
            SharedLedgerJournal::new(ledger, run_generation),
            artifacts,
            CoordinatorConfig {
                attach,
                foreign_diagnostics,
                fixed_model: session.fixed_model.clone(),
                default_effort: session.default_effort.clone(),
                supported_efforts: session.supported_efforts.iter().cloned().collect(),
                cwd: session.cwd.clone(),
                developer_instructions: session.developer_instructions.clone(),
                sandbox: session.sandbox.clone(),
                approval_policy: session.approval_policy.clone(),
                safety_policy: session.safety_policy,
                run_generation,
                server_key: session.server_key.clone(),
                server_epoch: session.server_epoch,
                run_id: facts.run_id,
                controller_id: session.controller_id,
                control_mode: session.control_mode.clone(),
            },
            &session.canonical_codex_home,
        );
        let mut coordinator = match coordinator {
            Ok(coordinator) => coordinator,
            Err(_) => {
                mailbox.stop();
                wire.shutdown();
                let _ = reader.join();
                let _ = stop_dedicated_server(&mut dedicated_server);
                return Err(WorkerProtocolError::AppServerUnavailable);
            }
        };
        coordinator.restore_durable_state(
            replayed.lifecycle,
            replayed.thread_id,
            replayed.active_turn_id,
            replayed.latest_turn_id,
        );
        coordinator
            .restore_interaction_resolutions(interaction_decisions)
            .map_err(|_| WorkerProtocolError::LedgerReplay)?;
        let progress = Arc::new(SessionProgress::new(facts.clone()));
        progress.publish(
            coordinator_control_state(&coordinator, false),
            coordinator.thread_id().map(str::to_owned),
            coordinator.active_turn_effort().map(str::to_owned),
            Vec::new(),
        );
        Ok(Self {
            facts,
            wire,
            mailbox,
            progress,
            coordinator: Mutex::new(Some(coordinator)),
            workers: Mutex::new(vec![reader]),
            dedicated_server: Mutex::new(dedicated_server),
        })
    }

    /// Start the worker-owned drain.
    ///
    /// From here the Run advances on its own thread: every accepted Turn is
    /// drained to its terminal whether or not a caller is waiting, and every
    /// mutation is performed there, after its Controller credential has been
    /// revalidated against the Run's current durable binding.
    pub fn begin(
        &self,
        identity: WorkerIdentity,
        control: Arc<Mutex<WorkerControlState>>,
        authority: Arc<RunControllerAuthority>,
    ) -> Result<(), WorkerProtocolError> {
        let coordinator = self
            .coordinator
            .lock()
            .map_err(|_| WorkerProtocolError::Io)?
            .take()
            .ok_or(WorkerProtocolError::Io)?;
        let mailbox = Arc::clone(&self.mailbox);
        let progress = Arc::clone(&self.progress);
        let drain = thread::spawn(move || {
            drain_run(
                coordinator,
                &identity,
                &mailbox,
                &progress,
                &control,
                &authority,
            );
        });
        self.workers
            .lock()
            .map_err(|_| WorkerProtocolError::Io)?
            .push(drain);
        Ok(())
    }

    #[must_use]
    pub fn control_state(&self) -> WorkerControlState {
        self.progress.control_state()
    }

    #[must_use]
    pub fn thread_id(&self) -> Option<String> {
        self.progress.thread_id()
    }

    #[must_use]
    pub fn dedicated_server_identity(&self) -> Option<DedicatedServerIdentity> {
        self.dedicated_server
            .lock()
            .ok()
            .and_then(|server| server.as_ref().map(|server| server.identity().clone()))
    }

    /// Start a Turn.  `Send` additionally waits on published Run state until
    /// that Turn reaches a terminal or opens an interaction.
    pub fn submit(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
        request: TurnControlRequest,
        delivery: DeliveryMode,
        caller_budget: Option<Duration>,
    ) -> ControlResponseV1 {
        // Acceptance itself is never cut short by the caller's own timeout:
        // abandoning a `turn/start` that may already have been accepted would
        // trade a bounded wait for an uncertain outcome.  The caller's budget
        // bounds what is left of the wait after the Turn exists.
        let deadline = caller_budget.and_then(|budget| Instant::now().checked_add(budget));
        let accepted = self.dispatch(
            operation,
            credential,
            SessionCommand::Accept { request, delivery },
        );
        match accepted {
            ControlResponseV1::Accepted { accepted } if delivery == DeliveryMode::Send => {
                let remaining = caller_budget.map(|budget| {
                    deadline.map_or(budget, |deadline| {
                        deadline.saturating_duration_since(Instant::now())
                    })
                });
                self.progress.await_turn(&accepted.turn_id, remaining)
            }
            other => other,
        }
    }

    pub fn external_submit(
        &self,
        credential: CredentialCarrier,
        engagement_id: Uuid,
        request: TurnControlRequest,
    ) -> ControlResponseV1 {
        self.dispatch_external(
            "engagement.assign",
            credential,
            engagement_id,
            SessionCommand::Accept {
                request,
                delivery: DeliveryMode::Submit,
            },
        )
    }

    /// Rejoin the named Turn.  Open to same-uid observers: it starts,
    /// answers, interrupts, and ends nothing.
    pub fn wait(&self, turn_id: &str, caller_budget: Option<Duration>) -> ControlResponseV1 {
        self.progress.await_named(turn_id, caller_budget)
    }

    /// The shutdown sequence ADR-011 requires, run before this worker stops.
    ///
    /// Returns whether a terminal was actually observed, which is the only
    /// thing that may be reported as `terminal_confirmed`.  With no Turn in
    /// flight there is nothing to confirm and nothing to lose, so it is
    /// trivially true; otherwise the Turn is interrupted through the drain,
    /// its terminal is waited for within the normative budget, and expiry is
    /// recorded durably as `outcome_unknown` rather than reported as an
    /// orderly stop.
    #[must_use]
    pub fn settle_before_shutdown(&self) -> bool {
        // Fence before reading which Turn is live.  A Turn accepted after this
        // point would be interrupted by nothing and waited for by nobody, so
        // shutdown has to be the last mutation this generation takes on rather
        // than merely the next one it happens to perform.
        self.mailbox.fence();
        // Ask the drain unconditionally instead of deciding from its last
        // published snapshot.  `perform(Accept)` may be inside `turn/start`
        // while the snapshot still says idle; queueing behind it makes the
        // interrupt decision authoritative and ordered against that accept.
        // Its own budget is separate from the terminal wait: docs/specs/README.md starts
        // the five seconds at "sends `turn/interrupt`", not before.
        let outcome = Arc::new(MutationOutcome::default());
        if self
            .mailbox
            .submit(SessionMutation {
                operation: "run.shutdown",
                credential: None,
                external_engagement: None,
                command: SessionCommand::ShutdownInterrupt,
                outcome: Arc::clone(&outcome),
            })
            .is_some()
        {
            return false;
        }
        // A mutation ahead of shutdown may itself be waiting on the
        // app-server.  If it does not release the drain within the shutdown
        // budget, close the transport so that already-written work is
        // quarantined by that mutation's normal lost-after-write path.  Do
        // not queue Abandon behind an interrupt that has not started: doing
        // so would give the Turn no post-interrupt terminal window at all.
        if !outcome.await_started(SHUTDOWN_TERMINAL_TIMEOUT) {
            self.wire.shutdown();
            let _ = outcome.await_settled(SHUTDOWN_TERMINAL_TIMEOUT);
            return false;
        }
        let interrupt_deadline = Instant::now()
            .checked_add(SHUTDOWN_TERMINAL_TIMEOUT)
            .unwrap_or_else(Instant::now);
        let interrupt = interrupt_deadline
            .checked_duration_since(Instant::now())
            .and_then(|remaining| outcome.await_settled(remaining));
        let turn_id = match interrupt {
            Some(ControlResponseV1::Interrupted { turn_id, .. }) => turn_id,
            Some(ControlResponseV1::Shutdown {
                terminal_confirmed: true,
                ..
            }) => return true,
            None => {
                self.wire.shutdown();
                return false;
            }
            _ => {
                let _ = self.dispatch_with(
                    "run.shutdown",
                    None,
                    SessionCommand::ShutdownAbandon,
                    SHUTDOWN_TERMINAL_TIMEOUT,
                );
                return false;
            }
        };
        let terminal_budget = interrupt_deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_default();
        if self
            .progress
            .await_terminal_evidence(&turn_id, terminal_budget)
        {
            return true;
        }
        // The Turn's outcome is now unobservable, and the ledger has to say so
        // before this generation goes away.  docs/specs/README.md makes that a
        // requirement, not an attempt: a refused dispatch, an unanswered one,
        // or a failed journal write all mean the record was never made, and a
        // shutdown that reports an unconfirmed terminal while leaving nothing
        // durable behind it is the fabrication ADR-011 forbids.  So the Run
        // fails here instead, under the invariant it could not keep.
        let recorded = self.dispatch_with(
            "run.shutdown",
            None,
            SessionCommand::ShutdownAbandon,
            SHUTDOWN_TERMINAL_TIMEOUT,
        );
        // A drain that had already given this Turn up — a transport that died
        // first takes the Run down the same path — refuses the dispatch
        // because the Run is over, not because the record is missing, and it
        // has published its own fault if the write is what failed.
        if !matches!(recorded, ControlResponseV1::Closed { .. }) && !self.progress.gave_up(&turn_id)
        {
            self.progress.fail(TurnError::Journal(format!(
                "run {} lost turn {turn_id} without recording outcome_unknown",
                self.facts.run_id
            )));
        }
        false
    }

    pub fn respond(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
        request_id: u64,
        idempotency_key: String,
        response: Value,
    ) -> ControlResponseV1 {
        self.dispatch(
            operation,
            credential,
            SessionCommand::Respond {
                request_id,
                idempotency_key,
                response,
            },
        )
    }

    pub fn interrupt(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
    ) -> ControlResponseV1 {
        self.dispatch(operation, credential, SessionCommand::Interrupt)
    }

    pub fn external_interrupt(
        &self,
        credential: CredentialCarrier,
        engagement_id: Uuid,
    ) -> ControlResponseV1 {
        self.dispatch_external(
            "engagement.cancel",
            credential,
            engagement_id,
            SessionCommand::Interrupt,
        )
    }

    pub fn pause(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
        interrupt: bool,
    ) -> ControlResponseV1 {
        self.dispatch(operation, credential, SessionCommand::Pause { interrupt })
    }

    pub fn resume(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
    ) -> ControlResponseV1 {
        self.dispatch(operation, credential, SessionCommand::Resume)
    }

    pub fn reconcile(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
    ) -> ControlResponseV1 {
        self.dispatch(operation, credential, SessionCommand::Reconcile)
    }

    pub fn settle_lifecycle_timeout(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
        close: bool,
    ) -> ControlResponseV1 {
        self.dispatch(
            operation,
            credential,
            SessionCommand::SettleLifecycleTimeout { close },
        )
    }

    pub fn close(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
        interrupt: bool,
    ) -> ControlResponseV1 {
        self.dispatch(operation, credential, SessionCommand::Close { interrupt })
    }

    pub fn external_close(
        &self,
        credential: CredentialCarrier,
        engagement_id: Uuid,
        interrupt: bool,
    ) -> ControlResponseV1 {
        self.dispatch_external(
            "engagement.release",
            credential,
            engagement_id,
            SessionCommand::Close { interrupt },
        )
    }

    pub fn set_writer_access(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
        write: bool,
        writer_generation: u64,
        transaction_id: Uuid,
    ) -> ControlResponseV1 {
        self.dispatch(
            operation,
            credential,
            SessionCommand::SetWriterAccess {
                write,
                writer_generation,
                transaction_id,
            },
        )
    }

    pub fn external_set_writer_access(
        &self,
        credential: CredentialCarrier,
        engagement_id: Uuid,
        write: bool,
        writer_generation: u64,
        transaction_id: Uuid,
    ) -> ControlResponseV1 {
        self.dispatch_external(
            if write {
                "engagement.acquire_write"
            } else {
                "engagement.release_write"
            },
            credential,
            engagement_id,
            SessionCommand::SetWriterAccess {
                write,
                writer_generation,
                transaction_id,
            },
        )
    }

    /// The last Turn this generation drained to a terminal, for `status`.
    #[must_use]
    pub fn last_terminal(&self) -> Option<TerminalTurn> {
        self.progress.last_terminal()
    }

    /// Report the Run's authoritative state to a fenced controller reset.
    ///
    /// It travels the drain queue like a mutation so it is ordered against
    /// every mutation, and it carries no Controller credential: the durable
    /// reset prepare is what authorizes it.
    pub fn reset_fence(&self, operation: &'static str, confirmation: Uuid) -> ControlResponseV1 {
        self.dispatch_with(
            operation,
            None,
            SessionCommand::ResetFence { confirmation },
            RESET_FENCE_TIMEOUT,
        )
    }

    fn dispatch(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
        command: SessionCommand,
    ) -> ControlResponseV1 {
        self.dispatch_with(operation, Some(credential), command, CONTROL_CALL_TIMEOUT)
    }

    fn dispatch_external(
        &self,
        operation: &'static str,
        credential: CredentialCarrier,
        engagement_id: Uuid,
        command: SessionCommand,
    ) -> ControlResponseV1 {
        let outcome = Arc::new(MutationOutcome::default());
        if let Some(refusal) = self.mailbox.submit(SessionMutation {
            operation,
            credential: Some(credential),
            external_engagement: Some(engagement_id),
            command,
            outcome: Arc::clone(&outcome),
        }) {
            return refusal;
        }
        outcome
            .await_settled(CONTROL_CALL_TIMEOUT)
            .unwrap_or_else(|| {
                run_busy(
                    &self.facts,
                    "turn",
                    "run did not reach the mutation within the control call budget",
                )
            })
    }

    fn dispatch_with(
        &self,
        operation: &'static str,
        credential: Option<CredentialCarrier>,
        command: SessionCommand,
        budget: Duration,
    ) -> ControlResponseV1 {
        let outcome = Arc::new(MutationOutcome::default());
        if let Some(refusal) = self.mailbox.submit(SessionMutation {
            operation,
            credential,
            external_engagement: None,
            command,
            outcome: Arc::clone(&outcome),
        }) {
            return refusal;
        }
        outcome.await_settled(budget).unwrap_or_else(|| {
            run_busy(
                &self.facts,
                "turn",
                "run did not reach the mutation within the control call budget",
            )
        })
    }

    /// Stop the drain and the reader within a bounded budget.
    ///
    /// The socket is shut down in both directions so a reader parked on it
    /// returns now rather than waiting out the app-server.
    pub fn shutdown(&self) {
        let context = self.facts.bare();
        for refused in self.mailbox.stop() {
            refused.outcome.settle(failed(
                &TurnError::transport(TransportStage::Shutdown, "run is stopping"),
                &context,
            ));
        }
        self.progress.stop();
        self.wire.shutdown();
        let workers = match self.workers.lock() {
            Ok(mut workers) => std::mem::take(&mut *workers),
            Err(_) => Vec::new(),
        };
        for worker in workers {
            let _ = worker.join();
        }
        if let Ok(mut owned) = self.dedicated_server.lock() {
            let _ = stop_dedicated_server(&mut owned);
        }
    }
}

#[cfg(target_os = "macos")]
fn start_dedicated_server(
    config: &DedicatedServerBootstrap,
) -> Result<OwnedDedicatedServer, WorkerProtocolError> {
    config.validate()?;
    let executable = Path::new(&config.argv[0]);
    let executable_metadata =
        fs::metadata(executable).map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    if executable_metadata.dev() != config.executable_device
        || executable_metadata.ino() != config.executable_inode
        || file_sha256(executable)? != config.executable_sha256
    {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let socket_parent = config
        .socket_path
        .parent()
        .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
    prepare_socket_root(DarwinSystem.current_uid(), socket_parent)?;
    if fs::symlink_metadata(&config.socket_path).is_ok() {
        return Err(WorkerProtocolError::SocketIdentityMismatch);
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&config.log_path)
        .map_err(|_| WorkerProtocolError::Io)?;
    let stderr = log.try_clone().map_err(|_| WorkerProtocolError::Io)?;
    let mut command = Command::new(executable);
    command
        .args(&config.argv[1..])
        .args(["app-server", "--listen"])
        .arg(format!("unix://{}", config.socket_path.display()))
        .current_dir(&config.cwd)
        .env_clear()
        .envs(&config.environment)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(stderr));
    let mut child = DarwinSystem
        .spawn_detached(&mut command)
        .map_err(|_| WorkerProtocolError::WorkerStartFailed)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .unwrap_or_else(Instant::now);
    loop {
        if let Ok(socket_metadata) = fs::symlink_metadata(&config.socket_path) {
            if !socket_metadata.file_type().is_socket()
                || socket_metadata.uid() != DarwinSystem.current_uid()
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(WorkerProtocolError::SocketIdentityMismatch);
            }
            if fs::set_permissions(&config.socket_path, fs::Permissions::from_mode(0o600)).is_err()
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(WorkerProtocolError::Io);
            }
            let process = match DarwinSystem.bsd_process_identity(child.id()) {
                Ok(process) => process,
                Err(_) => {
                    terminate_unpublished_child(&mut child);
                    return Err(WorkerProtocolError::InvalidIdentity);
                }
            };
            if process.zombie
                || process.process_group_id != child.id()
                || process.session_id != child.id()
                || process.uid != DarwinSystem.current_uid()
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(WorkerProtocolError::InvalidIdentity);
            }
            let executable_path = match DarwinSystem.realpath(executable) {
                Ok(path) => path,
                Err(_) => {
                    terminate_unpublished_child(&mut child);
                    return Err(WorkerProtocolError::InvalidIdentity);
                }
            };
            return Ok(OwnedDedicatedServer {
                child,
                identity: DedicatedServerIdentity {
                    pid: process.pid,
                    process_group_id: process.process_group_id,
                    session_id: process.session_id,
                    uid: process.uid,
                    start_tvsec: process.start_tvsec,
                    start_tvusec: process.start_tvusec,
                    executable_path,
                    executable_device: executable_metadata.dev(),
                    executable_inode: executable_metadata.ino(),
                    executable_sha256: config.executable_sha256.clone(),
                },
                socket_path: config.socket_path.clone(),
                socket_identity: SocketIdentity {
                    device: socket_metadata.dev(),
                    inode: socket_metadata.ino(),
                },
            });
        }
        match child.try_wait() {
            Ok(Some(_)) => return Err(WorkerProtocolError::WorkerStartFailed),
            Err(_) => {
                terminate_unpublished_child(&mut child);
                return Err(WorkerProtocolError::Io);
            }
            Ok(None) if Instant::now() >= deadline => {
                terminate_unpublished_child(&mut child);
                return Err(WorkerProtocolError::WorkerStartFailed);
            }
            Ok(None) => {}
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(target_os = "macos"))]
fn start_dedicated_server(
    _config: &DedicatedServerBootstrap,
) -> Result<OwnedDedicatedServer, WorkerProtocolError> {
    Err(WorkerProtocolError::WorkerStartFailed)
}

fn terminate_unpublished_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn stop_dedicated_server(
    server: &mut Option<OwnedDedicatedServer>,
) -> Result<BackgroundAbsenceEvidence, WorkerProtocolError> {
    let Some(mut server) = server.take() else {
        return Ok(BackgroundAbsenceEvidence {
            census_revision: 0,
            consecutive_empty_samples: 5,
        });
    };
    let controlled = classify_dedicated_server_identity(&server.identity)
        == ProcessIdentityVerdict::Match
        && dedicated_group_safe_to_signal(&server.identity);
    if controlled {
        let _ = DarwinSystem.signal_process_group(server.identity.process_group_id, libc::SIGTERM);
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .unwrap_or_else(Instant::now);
        while Instant::now() < deadline {
            if DarwinSystem
                .process_group_pids(server.identity.process_group_id)
                .is_ok_and(|members| members.is_empty())
            {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if DarwinSystem
            .process_group_pids(server.identity.process_group_id)
            .is_ok_and(|members| !members.is_empty())
            && dedicated_group_safe_to_signal(&server.identity)
        {
            let _ =
                DarwinSystem.signal_process_group(server.identity.process_group_id, libc::SIGKILL);
        }
    }
    if !controlled {
        let _ = server.child.try_wait();
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let total_deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .unwrap_or_else(Instant::now);
    while Instant::now() < total_deadline {
        if server
            .child
            .try_wait()
            .map_err(|_| WorkerProtocolError::Io)?
            .is_some()
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let mut empty_samples = 0_u8;
    for sample in 0..5_u8 {
        if !DarwinSystem
            .process_group_pids(server.identity.process_group_id)
            .is_ok_and(|members| members.is_empty())
        {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        empty_samples += 1;
        if sample < 4 {
            thread::sleep(Duration::from_millis(100));
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(&server.socket_path)
        && metadata.file_type().is_socket()
        && metadata.uid() == server.identity.uid
        && metadata.dev() == server.socket_identity.device
        && metadata.ino() == server.socket_identity.inode
    {
        fs::remove_file(&server.socket_path).map_err(|_| WorkerProtocolError::Io)?;
    }
    Ok(BackgroundAbsenceEvidence {
        census_revision: server.identity.start_tvsec,
        consecutive_empty_samples: empty_samples,
    })
}

fn dedicated_group_safe_to_signal(identity: &DedicatedServerIdentity) -> bool {
    let Ok(members) = DarwinSystem.process_group_pids(identity.process_group_id) else {
        return false;
    };
    let Ok(census) = DarwinSystem.all_process_identities() else {
        return false;
    };
    let by_pid = census
        .iter()
        .map(|process| (process.pid, process.parent_pid))
        .collect::<std::collections::BTreeMap<_, _>>();
    members.iter().all(|pid| {
        census
            .iter()
            .find(|process| process.pid == *pid)
            .is_some_and(|process| {
                process.uid == identity.uid
                    && (process.pid == identity.pid
                        || process.session_id == identity.session_id
                        || process_descends_from(process.pid, identity.pid, &by_pid))
            })
    })
}

impl Drop for WorkerSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Read the app-server on the Run's own thread, one granted message at a time.
///
/// The reader touches no Run state, so it can stay parked on the socket for as
/// long as the app-server is quiet without holding anything a control caller
/// needs.
fn read_app_server(wire: &DuplexConnection, mailbox: &SessionMailbox) {
    while mailbox.await_grant() {
        let message = wire.receive().map_err(TurnError::from);
        let lost = message.is_err();
        mailbox.deliver(message);
        if lost {
            break;
        }
    }
}

/// Own the Run: fold in every app-server message and perform every mutation.
fn drain_run(
    mut coordinator: SessionCoordinator,
    identity: &WorkerIdentity,
    mailbox: &SessionMailbox,
    progress: &SessionProgress,
    control: &Mutex<WorkerControlState>,
    authority: &RunControllerAuthority,
) {
    publish_state(&coordinator, progress, control);
    loop {
        match mailbox.next_work() {
            SessionWork::Stop => break,
            SessionWork::Message(Err(error)) => {
                // A transport that dies under an accepted Turn has cost this
                // Run that Turn's outcome, not merely its next message.  The
                // loss is recorded durably and restated as the uncertainty it
                // is, so no caller is handed a retryable transport hiccup for
                // a Turn that may still be running on the app-server.
                remember_lost(&coordinator, progress);
                let reported = coordinator.lost_in_flight(error);
                progress.fail(reported);
                publish_state(&coordinator, progress, control);
                break;
            }
            SessionWork::Message(Ok(message)) => {
                match coordinator.ingest(message) {
                    Ok(Some(terminal)) => progress.settle(terminal),
                    // A terminal this Run already published is noise on a
                    // shared server, not a second outcome.
                    Ok(None) | Err(TurnError::DuplicateTerminal) => {}
                    // A message this Run could not place is only fatal if it
                    // cost the Run the outcome of a Turn it had in flight.
                    // Otherwise it is noise on a shared server, and reporting
                    // it to every later caller would be the real fault.
                    Err(error) => {
                        if *coordinator.state() == CoordinatorState::OutcomeUnknown {
                            progress.fail(error);
                        }
                    }
                }
                publish_state(&coordinator, progress, control);
            }
            SessionWork::Mutation(mutation) => {
                let outcome = Arc::clone(&mutation.outcome);
                outcome.start();
                let response = perform(&mut coordinator, identity, progress, authority, mutation);
                publish_state(&coordinator, progress, control);
                outcome.settle(response);
            }
        }
    }
    // Whatever ended the drain, a Turn still in flight has just lost the only
    // thread that could observe it.  ADR-011 wants that in the ledger before
    // the generation goes away, so stopping records it exactly as an expired
    // shutdown does rather than dropping the coordinator on the floor and
    // leaving a Turn that ran with nothing durable to recover it from.
    remember_lost(&coordinator, progress);
    if let Err(error) = coordinator.abandon() {
        progress.fail(error);
    }
    publish_state(&coordinator, progress, control);
    let stopping = progress.facts.bare();
    for refused in mailbox.stop() {
        refused.outcome.settle(failed(
            &TurnError::transport(TransportStage::Shutdown, "run is stopping"),
            &stopping,
        ));
    }
    progress.stop();
}

/// Perform one mutation, proving Controller authority immediately before it.
fn perform(
    coordinator: &mut SessionCoordinator,
    identity: &WorkerIdentity,
    progress: &SessionProgress,
    authority: &RunControllerAuthority,
    mutation: SessionMutation,
) -> ControlResponseV1 {
    // ADR-016: the CLI's check was an early rejection and the caller's thread
    // only carried the descriptor here.  This is the authoritative check, on
    // the thread that performs the effect, with nothing between it and the
    // effect it guards.
    //
    // A fenced controller reset is the one piece of work whose authority is
    // not a Controller credential — it is the operation that replaces the
    // Controller — so it proves the operator's durable reset prepare instead,
    // in the same position: immediately before its effect, on this thread.
    if let SessionCommand::ResetFence { confirmation } = mutation.command {
        let lifecycle = coordinator_control_state(coordinator, progress.is_closed()).lifecycle;
        return match authority.reset_fence(confirmation, &lifecycle) {
            Ok(()) => ControlResponseV1::ResetFence {
                lifecycle: lifecycle.clone(),
                thread_id: coordinator.thread_id().map(str::to_owned),
                active_turn: coordinator.active_turn_id().map(str::to_owned),
                pending_interactions: coordinator.pending_interactions().len(),
            },
            Err(error) => refusal_from(error),
        };
    }
    // Shutdown's authority is the frozen control-v1 identity check, or this
    // process's own SIGTERM.  Neither carries a Controller credential, and
    // demanding one would mean a Run whose Controller was rotated could never
    // be stopped cleanly.
    if matches!(
        mutation.command,
        SessionCommand::ShutdownInterrupt | SessionCommand::ShutdownAbandon
    ) {
        let context = progress.facts.context(
            coordinator.thread_id().map(str::to_owned),
            coordinator.active_turn_id().map(str::to_owned),
        );
        return match mutation.command {
            SessionCommand::ShutdownAbandon => {
                remember_lost(coordinator, progress);
                match coordinator.abandon() {
                    Ok(()) => ControlResponseV1::Closed {
                        identity: identity.clone(),
                        thread_id: coordinator.thread_id().map(str::to_owned),
                    },
                    // A Run that cannot record the outcome it lost has broken
                    // a durable-write invariant, and that is not a detail of
                    // this one shutdown call: every caller still waiting on
                    // this Run has to be told, not just the one that asked.
                    Err(error) => {
                        progress.fail(error.clone());
                        failed(&error, &context)
                    }
                }
            }
            _ if coordinator.active_turn_id().is_none()
                && *coordinator.state() != CoordinatorState::OutcomeUnknown =>
            {
                ControlResponseV1::Shutdown {
                    identity: identity.clone(),
                    terminal_confirmed: true,
                }
            }
            _ => interrupt_active(coordinator, &context),
        };
    }
    let authorization = authority.authorize_mutation(
        mutation.operation,
        mutation.credential.as_ref(),
        mutation.external_engagement,
    );
    if let Err(error) = authorization {
        return refusal_from(error);
    }
    let operation = mutation.operation;
    let context = progress.facts.context(
        coordinator.thread_id().map(str::to_owned),
        coordinator.active_turn_id().map(str::to_owned),
    );
    match mutation.command {
        SessionCommand::Accept { request, delivery } => {
            // A closed Run refusing a new Turn is a lifecycle fact about the
            // Run, not a complaint about the caller's arguments: the identical
            // request was legal a moment ago and no rewording of it will ever
            // be accepted again.  docs/specs/README.md gives that its own registered code
            // and exit class, so the caller can tell "fix your input" from
            // "this Run is over".
            if progress.is_closed() {
                let state = coordinator_control_state(coordinator, true).lifecycle;
                return run_state_conflict(
                    &progress.facts,
                    &state,
                    operation,
                    format!("{operation} is refused because the run is {state}"),
                );
            }
            let turn = match turn_request(coordinator, &request, delivery) {
                Ok(turn) => turn,
                Err(error) => return failed(&error, &context),
            };
            match coordinator.accept(turn) {
                Ok(DeliveryResult::Terminal(terminal)) => {
                    progress.settle(terminal.clone());
                    terminal_response(terminal)
                }
                Ok(DeliveryResult::Accepted(accepted)) => ControlResponseV1::Accepted { accepted },
                Ok(DeliveryResult::WaitingInteraction { accepted, requests }) => {
                    ControlResponseV1::WaitingInteraction {
                        thread_id: accepted.thread_id,
                        turn_id: accepted.turn_id,
                        effort: accepted.effort,
                        requests,
                    }
                }
                Err(error) => failed(&error, &context),
            }
        }
        SessionCommand::Respond {
            request_id,
            idempotency_key,
            response,
        } => match coordinator.respond(request_id, idempotency_key, response) {
            Ok(resolution_receipt_id) => ControlResponseV1::Responded {
                request_id,
                resolution_receipt_id,
            },
            Err(error) => failed(&error, &context),
        },
        SessionCommand::Interrupt => interrupt_active(coordinator, &context),
        SessionCommand::Pause { interrupt } => {
            let live = coordinator.active_turn_id().is_some()
                || !coordinator.pending_interactions().is_empty();
            if live && !interrupt {
                let state = coordinator_control_state(coordinator, progress.is_closed()).lifecycle;
                return run_state_conflict(
                    &progress.facts,
                    &state,
                    "run.pause",
                    format!("run.pause requires --interrupt while the run is {state}"),
                );
            }
            if live {
                return interrupt_active(coordinator, &context);
            }
            match coordinator.pause(false) {
                Ok(()) => ControlResponseV1::Status {
                    identity: identity.clone(),
                    lifecycle: "paused".to_owned(),
                    active_turn: None,
                    last_terminal: progress.last_terminal(),
                },
                Err(error) => failed(&error, &context),
            }
        }
        SessionCommand::Resume => match coordinator.resume() {
            Ok(()) => ControlResponseV1::Status {
                identity: identity.clone(),
                lifecycle: "idle".to_owned(),
                active_turn: None,
                last_terminal: progress.last_terminal(),
            },
            Err(error) => failed(&error, &context),
        },
        SessionCommand::Reconcile => match coordinator.reconcile_history() {
            Ok(_) => ControlResponseV1::Status {
                identity: identity.clone(),
                lifecycle: coordinator_control_state(coordinator, false).lifecycle,
                active_turn: None,
                last_terminal: progress.last_terminal(),
            },
            Err(error) => failed(&error, &context),
        },
        SessionCommand::SettleLifecycleTimeout { close } => {
            remember_lost(coordinator, progress);
            if let Err(error) = coordinator.abandon() {
                return failed(&error, &context);
            }
            if close {
                if let Err(error) = coordinator.seal_closed(false) {
                    return failed(&error, &context);
                }
                progress.mark_closed();
                ControlResponseV1::Closed {
                    identity: identity.clone(),
                    thread_id: coordinator.thread_id().map(str::to_owned),
                }
            } else {
                match coordinator.pause(false) {
                    Ok(()) => ControlResponseV1::Status {
                        identity: identity.clone(),
                        lifecycle: "paused".to_owned(),
                        active_turn: None,
                        last_terminal: progress.last_terminal(),
                    },
                    Err(error) => failed(&error, &context),
                }
            }
        }
        SessionCommand::Close { interrupt } => {
            let thread_id = coordinator.thread_id().map(str::to_owned);
            let live = coordinator.active_turn_id().is_some()
                || !coordinator.pending_interactions().is_empty();
            // docs/specs/README.md: "Pause and close reject running or waiting runs unless
            // `--interrupt` is present."  Interrupting a Turn is an effect on
            // the app-server, so it happens only when the caller asked for it
            // by name.
            if live && !interrupt {
                let state = coordinator_control_state(coordinator, progress.is_closed()).lifecycle;
                return run_state_conflict(
                    &progress.facts,
                    &state,
                    "run.close",
                    format!("run.close requires --interrupt while the run is {state}"),
                );
            }
            if live {
                return interrupt_active(coordinator, &context);
            }
            if let Err(error) = coordinator.seal_closed(false) {
                return failed(&error, &context);
            }
            progress.mark_closed();
            ControlResponseV1::Closed {
                identity: identity.clone(),
                thread_id,
            }
        }
        SessionCommand::SetWriterAccess {
            write,
            writer_generation,
            transaction_id,
        } => match coordinator.set_writer_access(write, writer_generation, transaction_id) {
            Ok(()) => ControlResponseV1::WriterAccessChanged {
                write,
                writer_generation,
                thread_id: coordinator.thread_id().map(str::to_owned),
            },
            Err(error) => failed(&error, &context),
        },
        // Answered above, before any Controller revalidation.
        SessionCommand::ResetFence { .. }
        | SessionCommand::ShutdownInterrupt
        | SessionCommand::ShutdownAbandon => {
            internal_invariant("unauthenticated work reached the effects")
        }
    }
}

/// Remember which Turn this Run is about to give up, while it is still the
/// active one.
///
/// The ledger records *that* an outcome was lost; this records *which*.  A
/// caller that comes back for that Turn is asking about a Turn this Run really
/// had, so it is owed the uncertainty rather than `TURN_NOT_FOUND` — which
/// would tell it the opposite of what happened.
fn remember_lost(coordinator: &SessionCoordinator, progress: &SessionProgress) {
    if let Some(lost) = coordinator.active_turn_id() {
        progress.abandon(lost.to_owned());
    }
}

/// Interrupt whatever Turn is live, naming the Turn the interrupt was aimed at.
fn interrupt_active(
    coordinator: &mut SessionCoordinator,
    context: &TurnFailureContext,
) -> ControlResponseV1 {
    let thread_id = coordinator.thread_id().unwrap_or_default().to_owned();
    let turn_id = coordinator.active_turn_id().unwrap_or_default().to_owned();
    let effort = coordinator
        .active_turn_effort()
        .unwrap_or_default()
        .to_owned();
    match coordinator.interrupt() {
        Ok(()) => ControlResponseV1::Interrupted {
            thread_id,
            turn_id,
            effort,
        },
        Err(error) => failed(&error, context),
    }
}

/// The registered refusal for a verb the Run's current lifecycle forbids.
///
/// The message says which lifecycle rule was broken, because "conflict" alone
/// leaves a caller guessing between "interrupt it first" and "this Run is
/// over"; the details stay the code's own closed members either way.
fn run_state_conflict(
    facts: &RunFacts,
    state: &str,
    operation: &str,
    message: String,
) -> ControlResponseV1 {
    ControlResponseV1::Failed {
        code: "RUN_STATE_CONFLICT".to_owned(),
        message,
        retryable: false,
        details: serde_json::json!({
            "run_id": facts.run_id,
            "state": state,
            "operation": operation,
        }),
    }
}

fn internal_invariant(invariant: &str) -> ControlResponseV1 {
    ControlResponseV1::Failed {
        code: "INTERNAL_ERROR".to_owned(),
        message: "internal invariant failed".to_owned(),
        retryable: false,
        details: serde_json::json!({"invariant": invariant}),
    }
}

/// Build one Turn from a control request, capturing every image before the
/// Turn is allowed to have any effect.
fn turn_request(
    coordinator: &SessionCoordinator,
    request: &TurnControlRequest,
    delivery: DeliveryMode,
) -> Result<TurnRequest, TurnError> {
    let mut images = Vec::with_capacity(request.images.len());
    for image in &request.images {
        images.push(ImageSnapshot::capture(&image.path, image.detail)?);
    }
    Ok(TurnRequest {
        idempotency_key: request.idempotency_key.clone(),
        message: request.message.clone(),
        images,
        model: coordinator.fixed_model().to_owned(),
        effort: request.effort.clone(),
        delivery,
    })
}

fn coordinator_control_state(coordinator: &SessionCoordinator, closed: bool) -> WorkerControlState {
    let lifecycle = if closed {
        "closed"
    } else {
        match coordinator.state() {
            CoordinatorState::Threadless | CoordinatorState::Idle => "idle",
            CoordinatorState::Paused => "paused",
            CoordinatorState::Running => "running",
            CoordinatorState::WaitingInteraction => "waiting_interaction",
            CoordinatorState::OutcomeUnknown => "outcome_unknown",
        }
    };
    WorkerControlState {
        lifecycle: lifecycle.to_owned(),
        active_turn: coordinator.active_turn_id().map(str::to_owned),
    }
}

/// Publish what the Run now is, both to waiting callers and to the control
/// socket's own status, so `status` and `events` advance under a Turn nobody
/// is waiting on.
fn publish_state(
    coordinator: &SessionCoordinator,
    progress: &SessionProgress,
    control: &Mutex<WorkerControlState>,
) {
    let state = coordinator_control_state(coordinator, progress.is_closed());
    if let Ok(mut published) = control.lock() {
        published.clone_from(&state);
    }
    progress.publish(
        state,
        coordinator.thread_id().map(str::to_owned),
        coordinator.active_turn_effort().map(str::to_owned),
        coordinator
            .pending_interactions()
            .into_iter()
            .cloned()
            .collect(),
    );
}

fn terminal_response(terminal: TerminalTurn) -> ControlResponseV1 {
    ControlResponseV1::Terminal { terminal }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupOwnerRecord {
    pub schema_version: u32,
    pub slot: u8,
    pub identity: WorkerIdentity,
    pub executable_path_sha256: String,
}

impl StartupOwnerRecord {
    pub fn encode_slot(&self) -> Result<[u8; STARTUP_SLOT_BYTES], WorkerProtocolError> {
        self.identity.validate()?;
        if self.schema_version != 1
            || self.slot > 1
            || decode_sha256(&self.executable_path_sha256).is_none()
        {
            return Err(WorkerProtocolError::InvalidOwnerRecord);
        }
        let payload =
            serde_json::to_vec(self).map_err(|_| WorkerProtocolError::InvalidOwnerRecord)?;
        if payload.len() > STARTUP_SLOT_BYTES - 2 - 32 {
            return Err(WorkerProtocolError::InvalidOwnerRecord);
        }
        let length =
            u16::try_from(payload.len()).map_err(|_| WorkerProtocolError::InvalidOwnerRecord)?;
        let mut slot = [0_u8; STARTUP_SLOT_BYTES];
        slot[..2].copy_from_slice(&length.to_be_bytes());
        slot[2..2 + payload.len()].copy_from_slice(&payload);
        let mut hasher = Sha256::new();
        hasher.update(OWNER_RECORD_DOMAIN);
        hasher.update(&slot[..STARTUP_SLOT_BYTES - 32]);
        slot[STARTUP_SLOT_BYTES - 32..].copy_from_slice(&hasher.finalize());
        Ok(slot)
    }

    pub fn decode_slot(
        slot: &[u8; STARTUP_SLOT_BYTES],
    ) -> Result<Option<Self>, WorkerProtocolError> {
        if slot.iter().all(|byte| *byte == 0) {
            return Ok(None);
        }
        let mut hasher = Sha256::new();
        hasher.update(OWNER_RECORD_DOMAIN);
        hasher.update(&slot[..STARTUP_SLOT_BYTES - 32]);
        if hasher.finalize().as_slice() != &slot[STARTUP_SLOT_BYTES - 32..] {
            return Err(WorkerProtocolError::InvalidOwnerRecord);
        }
        let length = usize::from(u16::from_be_bytes([slot[0], slot[1]]));
        if length == 0 || length > STARTUP_SLOT_BYTES - 2 - 32 {
            return Err(WorkerProtocolError::InvalidOwnerRecord);
        }
        if slot[2 + length..STARTUP_SLOT_BYTES - 32]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(WorkerProtocolError::InvalidOwnerRecord);
        }
        let record: Self = serde_json::from_slice(&slot[2..2 + length])
            .map_err(|_| WorkerProtocolError::InvalidOwnerRecord)?;
        record.encode_slot()?;
        Ok(Some(record))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerProtocolError {
    FrameTooLarge {
        observed_bytes: u64,
    },
    MalformedFrame,
    InvalidIdentity,
    InvalidRuntimeRecord,
    InvalidOwnerRecord,
    /// The CLI and the worker answering this Run are not the same Dolgorae
    /// build, or the request names a different worker.  Both protocol numbers
    /// travel with the failure because the checked contract requires the
    /// envelope to name them.
    ProtocolMismatch {
        expected_protocol: u32,
        actual_protocol: u32,
    },
    SocketIdentityMismatch,
    StartupBusy,
    LedgerReplay,
    WorkerStartFailed,
    AppServerUnavailable,
    Io,
}

impl WorkerProtocolError {
    /// The registered error code this failure is reported under.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::FrameTooLarge { .. } => "PROTOCOL_FRAME_TOO_LARGE",
            Self::ProtocolMismatch { .. } => "DOLGORAE_PROTOCOL_MISMATCH",
            Self::SocketIdentityMismatch
            | Self::InvalidRuntimeRecord
            | Self::InvalidOwnerRecord => "RUNTIME_PATH_COLLISION",
            Self::StartupBusy => "RUN_BUSY",
            Self::LedgerReplay => "AUDIT_INTEGRITY_FAILURE",
            Self::WorkerStartFailed
            | Self::AppServerUnavailable
            | Self::MalformedFrame
            | Self::InvalidIdentity
            | Self::Io => "TRANSPORT_FAILURE",
        }
    }

    /// Restate this failure as the checked machine error for one Run.
    ///
    /// `subject` is the runtime pathname whose identity the failure is a
    /// statement about — the Run's runtime record — because the contract
    /// requires every `RUNTIME_PATH_COLLISION` to name the path that
    /// collided, and no such failure is reportable without one.
    ///
    /// Transport failures stay non-retryable here and therefore report an
    /// `uncertain` acceptance: from the CLI's side of a private worker socket
    /// a lost frame may or may not have taken effect, and the contract admits
    /// only that pair.
    #[must_use]
    pub fn machine_error(self, run_id: Uuid, subject: &Path) -> MachineError {
        let code = self.code();
        let path = subject.to_string_lossy().into_owned();
        let (message, details) = match self {
            Self::FrameTooLarge { observed_bytes } => (
                "private worker frame exceeds the v1 byte limit",
                serde_json::json!({
                    "channel": "cli_worker",
                    "observed_bytes": observed_bytes,
                    "limit_bytes": MAX_CLI_WORKER_FRAME_BYTES,
                }),
            ),
            Self::ProtocolMismatch {
                expected_protocol,
                actual_protocol,
            } => (
                "private worker identity or protocol does not match",
                serde_json::json!({
                    "expected_protocol": expected_protocol,
                    "actual_protocol": actual_protocol,
                    "control_v1_available": true,
                }),
            ),
            Self::SocketIdentityMismatch => (
                "private worker runtime identity is unsafe or inconsistent",
                runtime_collision_details(&path, "worker_socket"),
            ),
            Self::InvalidRuntimeRecord => (
                "private worker runtime identity is unsafe or inconsistent",
                runtime_collision_details(&path, "worker_runtime_record"),
            ),
            Self::InvalidOwnerRecord => (
                "private worker runtime identity is unsafe or inconsistent",
                runtime_collision_details(&path, "startup_owner_record"),
            ),
            Self::StartupBusy => (
                "another process owns worker startup",
                serde_json::json!({"run_id": run_id, "owner_kind": "startup"}),
            ),
            Self::LedgerReplay => (
                "worker ledger replay failed",
                serde_json::json!({
                    "run_id": run_id,
                    "sequence": 0,
                    "reason": "worker ledger replay failed",
                }),
            ),
            Self::WorkerStartFailed => {
                ("hidden worker startup failed", transport_details("connect"))
            }
            Self::AppServerUnavailable => (
                "worker could not own the profile app-server session",
                transport_details("connect"),
            ),
            Self::MalformedFrame => (
                "private worker transport failed",
                transport_details("decode"),
            ),
            Self::InvalidIdentity => (
                "private worker transport failed",
                transport_details("correlate"),
            ),
            Self::Io => ("private worker transport failed", transport_details("read")),
        };
        // `StartupBusy` is the one retryable member of this family: another
        // process owns startup right now and the caller may come back.
        let retryable = matches!(self, Self::StartupBusy);
        MachineError::new(code, message, retryable, details)
    }
}

/// A runtime pathname whose identity no longer matches what the Run recorded.
fn runtime_collision_details(path: &str, subject: &str) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "expected_identity": {"subject": subject, "state": "recorded"},
        "observed_identity": {"subject": subject, "state": "unverifiable"},
    })
}

/// A non-retryable private-worker transport failure.
fn transport_details(stage: &str) -> serde_json::Value {
    serde_json::json!({"stage": stage, "acceptance": "uncertain", "request_id": Value::Null})
}

/// The Dolgorae build a process is running, as an ordinary request compares it.
///
/// It travels on every non-frozen control request: docs/specs/README.md has the CLI-worker
/// handshake carry the Dolgorae semantic version and binary SHA-256 and
/// refuse a mismatch, and a caller-side check alone cannot do that — an older
/// CLI that never learned to check simply would not perform it.  The worker
/// therefore refuses skew itself, from the build the caller declares.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutingBuild {
    pub version: String,
    pub mutation_protocol_version: u32,
    pub binary_sha256: String,
}

/// This process's own build.
pub fn current_dolgorae_build() -> Result<ExecutingBuild, WorkerProtocolError> {
    let executable = std::env::current_exe().map_err(|_| WorkerProtocolError::Io)?;
    Ok(ExecutingBuild {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        mutation_protocol_version: WORKER_PROTOCOL_VERSION,
        binary_sha256: file_sha256(&executable)?,
    })
}

pub fn worker_socket_path(
    uid: u32,
    workspace_id: &str,
    run_id: Uuid,
) -> Result<PathBuf, WorkerProtocolError> {
    let workspace = decode_sha256(workspace_id).ok_or(WorkerProtocolError::InvalidIdentity)?;
    if run_id.get_version_num() != 7 {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let mut hasher = Sha256::new();
    hasher.update(SOCKET_DOMAIN);
    hasher.update(workspace);
    hasher.update(run_id.as_bytes());
    let digest = hasher.finalize();
    let name = BASE32_NOPAD.encode(&digest[..20]);
    Ok(PathBuf::from(format!("/tmp/dolgorae-{uid}/s/{name}.sock")))
}

pub fn dedicated_server_socket_path(
    uid: u32,
    run_id: Uuid,
    generation: u64,
) -> Result<PathBuf, WorkerProtocolError> {
    if run_id.get_version_num() != 7 || generation == 0 {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"dolgorae-dedicated-server-socket-v1\0");
    hasher.update(run_id.as_bytes());
    hasher.update(generation.to_be_bytes());
    let digest = hasher.finalize();
    let name = BASE32_NOPAD.encode(&digest[..20]);
    Ok(PathBuf::from(format!("/tmp/dolgorae-{uid}/c/{name}.sock")))
}

pub fn bind_worker_socket(
    identity: &WorkerIdentity,
    stale: Option<VerifiedStaleSocket>,
) -> Result<WorkerSocketLease, WorkerProtocolError> {
    identity.validate()?;
    let path = worker_socket_path(identity.uid, &identity.workspace_id, identity.run_id)?;
    let parent = path.parent().ok_or(WorkerProtocolError::InvalidIdentity)?;
    prepare_socket_root(identity.uid, parent)?;
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        let Some(stale) = stale else {
            return Err(WorkerProtocolError::SocketIdentityMismatch);
        };
        if stale.path != path
            || !metadata.file_type().is_socket()
            || metadata.uid() != identity.uid
            || metadata.dev() != stale.identity.device
            || metadata.ino() != stale.identity.inode
        {
            return Err(WorkerProtocolError::SocketIdentityMismatch);
        }
        fs::remove_file(&path).map_err(|_| WorkerProtocolError::Io)?;
    } else if stale.is_some() {
        return Err(WorkerProtocolError::SocketIdentityMismatch);
    }
    let listener = UnixListener::bind(&path).map_err(|_| WorkerProtocolError::Io)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|_| WorkerProtocolError::Io)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| WorkerProtocolError::Io)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != identity.uid
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(WorkerProtocolError::SocketIdentityMismatch);
    }
    Ok(WorkerSocketLease {
        listener,
        path,
        identity: SocketIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    })
}

pub fn runtime_record_path(
    runtime_root: &Path,
    run_id: Uuid,
) -> Result<PathBuf, WorkerProtocolError> {
    if !runtime_root.is_absolute() || run_id.get_version_num() != 7 {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    Ok(runtime_root.join("runs").join(format!("{run_id}.json")))
}

pub fn write_worker_bootstrap(
    path: &Path,
    bootstrap: &WorkerBootstrap,
    uid: u32,
) -> Result<(), WorkerProtocolError> {
    bootstrap.validate()?;
    let parent = path
        .parent()
        .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| WorkerProtocolError::Io)?;
    if !metadata.file_type().is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    let bytes =
        serde_json::to_vec(bootstrap).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| WorkerProtocolError::Io)?;
    file.write_all(&bytes)
        .map_err(|_| WorkerProtocolError::Io)?;
    file.write_all(b"\n").map_err(|_| WorkerProtocolError::Io)?;
    file.sync_all().map_err(|_| WorkerProtocolError::Io)?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| WorkerProtocolError::Io)
}

pub fn read_worker_bootstrap(
    path: &Path,
    uid: u32,
) -> Result<WorkerBootstrap, WorkerProtocolError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > 64 * 1024
    {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    let bytes = fs::read(path).map_err(|_| WorkerProtocolError::Io)?;
    let bootstrap: WorkerBootstrap =
        serde_json::from_slice(&bytes).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    bootstrap.validate()?;
    Ok(bootstrap)
}

pub fn write_startup_handoff(handoff: &StartupHandoff) -> Result<(), WorkerProtocolError> {
    let bytes = serde_json::to_vec(handoff).map_err(|_| WorkerProtocolError::MalformedFrame)?;
    if bytes.len() > MAX_STARTUP_HANDOFF_BYTES {
        return Err(frame_too_large(bytes.len()));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .open(format!("/dev/fd/{STARTUP_HANDOFF_FD}"))
        .map_err(|_| WorkerProtocolError::Io)?;
    file.write_all(&bytes)
        .map_err(|_| WorkerProtocolError::Io)?;
    file.write_all(b"\n").map_err(|_| WorkerProtocolError::Io)?;
    file.flush().map_err(|_| WorkerProtocolError::Io)
}

pub fn read_startup_handoff<R: BufRead>(
    reader: &mut R,
) -> Result<StartupHandoff, WorkerProtocolError> {
    let mut bytes = Vec::new();
    let mut bounded = std::io::Read::take(
        std::io::Read::by_ref(reader),
        (MAX_STARTUP_HANDOFF_BYTES + 2) as u64,
    );
    let read = bounded
        .read_until(b'\n', &mut bytes)
        .map_err(|_| WorkerProtocolError::Io)?;
    if read == 0 || bytes.last() != Some(&b'\n') {
        return Err(WorkerProtocolError::MalformedFrame);
    }
    bytes.pop();
    if bytes.len() > MAX_STARTUP_HANDOFF_BYTES {
        return Err(frame_too_large(bytes.len()));
    }
    serde_json::from_slice(&bytes).map_err(|_| WorkerProtocolError::MalformedFrame)
}

/// Read the worker's `Bound` frame under the bind budget, then widen the
/// channel to the replay budget before the caller waits for `Ready`.
///
/// The two phases cost different things: binding a socket and writing a runtime
/// record is local setup, while reaching `Ready` also replays the durable
/// ledger and opens the app-server session, whose cost grows with Run history.
/// Holding replay to the bind budget would kill healthy workers with long
/// histories, and widening the bind budget would hide a worker that is stuck
/// before it ever owned anything.
pub fn read_bound_handoff(
    startup: UnixStream,
) -> Result<(std::io::BufReader<UnixStream>, WorkerRuntimeRecord), WorkerProtocolError> {
    startup
        .set_read_timeout(Some(STARTUP_BOUND_TIMEOUT))
        .map_err(|_| WorkerProtocolError::Io)?;
    let mut reader = std::io::BufReader::new(startup);
    let bound = match read_startup_handoff(&mut reader)? {
        StartupHandoff::Bound { record } => record,
        StartupHandoff::Failed { code } => return Err(startup_handoff_error(&code)),
        StartupHandoff::Ready { .. } => return Err(WorkerProtocolError::MalformedFrame),
    };
    reader
        .get_ref()
        .set_read_timeout(Some(STARTUP_READY_TIMEOUT))
        .map_err(|_| WorkerProtocolError::Io)?;
    Ok((reader, bound))
}

pub fn read_ready_handoff(
    reader: &mut std::io::BufReader<UnixStream>,
) -> Result<WorkerRuntimeRecord, WorkerProtocolError> {
    match read_startup_handoff(reader)? {
        StartupHandoff::Ready { record } => Ok(record),
        StartupHandoff::Failed { code } => Err(startup_handoff_error(&code)),
        StartupHandoff::Bound { .. } => Err(WorkerProtocolError::MalformedFrame),
    }
}

fn startup_handoff_error(code: &str) -> WorkerProtocolError {
    match code {
        "AUDIT_INTEGRITY_FAILURE" => WorkerProtocolError::LedgerReplay,
        "RUNTIME_PATH_COLLISION" => WorkerProtocolError::InvalidRuntimeRecord,
        _ => WorkerProtocolError::WorkerStartFailed,
    }
}

#[cfg(target_os = "macos")]
fn spawn_hidden_worker_inner(
    executable: &Path,
    bootstrap_path: &Path,
    held_startup_range: StartupLockFile,
) -> Result<StartedWorker, WorkerProtocolError> {
    let uid = DarwinSystem.current_uid();
    let startup_result = (|| {
        if !executable.is_absolute() || !bootstrap_path.is_absolute() {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        let bootstrap = read_worker_bootstrap(bootstrap_path, uid)?;
        if held_startup_range.path != bootstrap.startup_lock_path {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        let mut command = Command::new(executable);
        command
            .arg("__worker")
            .arg("--bootstrap")
            .arg(bootstrap_path);
        let (child, startup) = DarwinSystem
            .spawn_detached_with_fd3(&mut command)
            .map_err(|_| WorkerProtocolError::Io)?;
        let (reader, bound) = read_bound_handoff(startup)?;
        Ok((child, reader, bound, bootstrap))
    })();
    let release = held_startup_range.release_startup_range();
    let (mut child, mut reader, bound, bootstrap) = startup_result?;
    release?;
    if bound.identity.workspace_id != bootstrap.workspace_id
        || bound.identity.run_id != bootstrap.run_id
        || bound.identity.run_generation != bootstrap.run_generation
        || bound.identity.boot_uuid != bootstrap.boot_uuid
        || bound.identity.uid != uid
        || bound.identity.executable_sha256 != bootstrap.executable_sha256
    {
        return Err(WorkerProtocolError::ProtocolMismatch {
            expected_protocol: WORKER_PROTOCOL_VERSION,
            actual_protocol: WORKER_PROTOCOL_VERSION,
        });
    }
    let ready = read_ready_handoff(&mut reader)?;
    if bound.identity != ready.identity
        || bound.socket_path != ready.socket_path
        || bound.socket_identity != ready.socket_identity
        || bound.control_socket_epoch != ready.control_socket_epoch
        || bound.app_server_epoch != ready.app_server_epoch
        || bound.dolgorae_version != ready.dolgorae_version
        || bound.mutation_protocol_version != ready.mutation_protocol_version
        || bound.binary_sha256 != ready.binary_sha256
        || ready.identity.pid != child.id()
        || ready.identity.process_group_id != child.id()
    {
        return Err(WorkerProtocolError::ProtocolMismatch {
            expected_protocol: WORKER_PROTOCOL_VERSION,
            actual_protocol: WORKER_PROTOCOL_VERSION,
        });
    }
    if child
        .try_wait()
        .map_err(|_| WorkerProtocolError::Io)?
        .is_some()
    {
        return Err(WorkerProtocolError::WorkerStartFailed);
    }
    Ok(StartedWorker {
        child,
        record: ready,
    })
}

#[cfg(target_os = "macos")]
pub fn run_hidden_worker(bootstrap_path: &Path) -> Result<(), WorkerProtocolError> {
    if !bootstrap_path.is_absolute() {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    DarwinSystem
        .install_worker_signal_policy()
        .map_err(|_| WorkerProtocolError::Io)?;
    let uid = DarwinSystem.current_uid();
    let bootstrap = read_worker_bootstrap(bootstrap_path, uid)?;
    let process = DarwinSystem
        .current_process()
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    let identity = current_worker_identity(&bootstrap, process)?;
    identity.validate()?;
    if identity.pid != identity.process_group_id {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let startup_lock = StartupLockFile::open(&bootstrap.startup_lock_path, uid)?;
    let owner = StartupOwnerRecord {
        schema_version: 1,
        slot: 1,
        identity: identity.clone(),
        executable_path_sha256: bootstrap.executable_path_sha256.clone(),
    };
    let owner_guard = startup_lock.claim(&owner, Duration::ZERO)?;
    let lease = bind_worker_socket(&identity, None)?;
    let mut record = WorkerRuntimeRecord {
        schema_version: 1,
        identity: identity.clone(),
        socket_path: lease.path().to_owned(),
        socket_identity: lease.identity().clone(),
        control_socket_epoch: bootstrap.control_socket_epoch,
        app_server_epoch: bootstrap
            .session
            .as_ref()
            .map(|session| session.server_epoch),
        dedicated_server_identity: None,
        dolgorae_version: bootstrap.dolgorae_version,
        mutation_protocol_version: bootstrap.mutation_protocol_version,
        binary_sha256: bootstrap.executable_sha256,
    };
    write_runtime_record(&bootstrap.runtime_record_path, &record)?;
    write_startup_handoff(&StartupHandoff::Bound {
        record: record.clone(),
    })?;
    let facts = RunFacts {
        run_id: bootstrap.run_id,
        profile: bootstrap.profile.clone(),
    };
    let control = WorkerControlServer::new(
        WorkerHello {
            schema_version: 1,
            identity,
            control_socket_epoch: record.control_socket_epoch,
            dolgorae_version: record.dolgorae_version.clone(),
            mutation_protocol_version: record.mutation_protocol_version,
            binary_sha256: record.binary_sha256.clone(),
        },
        facts.clone(),
        WorkerControlState {
            lifecycle: "replaying".to_owned(),
            active_turn: None,
        },
        RunControllerAuthority::new(bootstrap.state_root.clone(), bootstrap.run_id),
    );
    let signal_control = control.clone();
    thread::spawn(move || {
        if DarwinSystem.wait_for_worker_sigterm().is_ok() {
            let _ = signal_control.terminate();
        }
    });
    let result = thread::scope(|scope| {
        let serving = control.clone();
        let serving_thread = scope.spawn(|| serving.serve(&lease));
        let startup = (|| {
            // Conformance verification owns the ledger only for replay.  It is
            // released before the Run's writer handle opens, because the audit
            // file admits exactly one writer at a time.
            let replayed = {
                let conformant = ConformantLedger::open(&bootstrap.ledger_root, bootstrap.run_id)
                    .map_err(|_| WorkerProtocolError::LedgerReplay)?;
                conformant
                    .inner()
                    .projection()
                    .map_err(|_| WorkerProtocolError::LedgerReplay)?
            };
            if control.is_stopping() {
                return Err(WorkerProtocolError::WorkerStartFailed);
            }
            let lifecycle = serde_json::to_value(replayed.lifecycle)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or(WorkerProtocolError::LedgerReplay)?;
            control.replace_state(WorkerControlState {
                lifecycle,
                active_turn: replayed.active_turn_id,
            })?;
            let ledger = Arc::new(Mutex::new(
                Ledger::open(&bootstrap.ledger_root, bootstrap.run_id)
                    .map_err(|_| WorkerProtocolError::LedgerReplay)?,
            ));
            if let Some(session) = &bootstrap.session {
                let owned = WorkerSession::connect(
                    session,
                    facts.clone(),
                    Arc::clone(&ledger),
                    bootstrap.run_generation,
                    uid,
                    &bootstrap.state_root,
                )?;
                record.dedicated_server_identity = owned.dedicated_server_identity();
                write_runtime_record(&bootstrap.runtime_record_path, &record)?;
                control.replace_state(owned.control_state())?;
                control.attach_run(Arc::new(owned), Arc::clone(&ledger))?;
            } else {
                control.attach_ledger(Arc::clone(&ledger))?;
            }
            write_startup_handoff(&StartupHandoff::Ready {
                record: record.clone(),
            })?;
            DarwinSystem
                .close_startup_fd3()
                .map_err(|_| WorkerProtocolError::Io)?;
            Ok(ledger)
        })();
        let ledger = match startup {
            Ok(ledger) => ledger,
            Err(error) => {
                control.stop();
                let _ = serving_thread.join();
                return Err(error);
            }
        };
        let served = serving_thread.join().map_err(|_| WorkerProtocolError::Io)?;
        control.detach_run()?;
        drop(ledger);
        served
    });
    let release = owner_guard.release();
    result?;
    release
}

/// Where a workspace keeps its per-Run worker discovery state.
#[must_use]
pub fn runtime_root(state_root: &Path) -> PathBuf {
    state_root.join("runtime")
}

/// Files a Run's foreign-thread observations in its Runtime Profile's durable
/// diagnostic journal.
///
/// The worker owns this composition because it is the only place that knows
/// both the Run's coordinator and, from its own session bootstrap, which
/// Runtime Profile the Run is pinned to.  The routing metadata is operational:
/// docs/specs/README.md puts "foreign-thread routing metadata" in the projection that
/// requires the operator capability, so the record is marked accordingly and
/// carries no request payload.
struct ProfileForeignDiagnostics {
    profile_root: PathBuf,
}

impl ForeignDiagnostics for ProfileForeignDiagnostics {
    fn record(
        &self,
        ignored: &IgnoredForeignRequest,
        lane: ForeignLane<'_>,
    ) -> Result<(), TurnError> {
        crate::profile::append_profile_diagnostic(
            &self.profile_root,
            "foreign_thread_request_ignored",
            serde_json::json!({
                "request_id": ignored.request_id,
                "method": ignored.method,
                "thread_id": ignored.thread_id,
                "turn_id": ignored.turn_id,
                "run_generation": lane.run_generation,
                "server_key": lane.server_key,
                "server_epoch": lane.server_epoch,
            }),
            crate::profile::DiagnosticProjection::Operational,
        )
        .map_err(|error| TurnError::Journal(error.message))
    }
}

/// Where this Run's Runtime Profile keeps its durable diagnostic journal.
///
/// The worker never re-derives which profile a Run belongs to: the server key
/// comes from the session bootstrap it was launched with, and the Application
/// Dolgorae home is the parent of the workspace state root it was pinned to.
/// A Run whose state root is not the expected two levels below Dolgorae home
/// has an unusable durable layout, which is a bootstrap fault rather
/// than something to guess around.
fn profile_diagnostics_root(
    state_root: &Path,
    server_key: &str,
) -> Result<PathBuf, WorkerProtocolError> {
    if decode_sha256(server_key).is_none() {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    let dolgorae_home = state_root
        .parent()
        .and_then(Path::parent)
        .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
    if !dolgorae_home.is_absolute() {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    Ok(dolgorae_home.join("profiles").join(server_key))
}

#[must_use]
pub fn startup_lock_path(runtime_root: &Path, run_id: Uuid) -> PathBuf {
    runtime_root
        .join("locks")
        .join("startup")
        .join(format!("{run_id}.lock"))
}

#[must_use]
pub fn bootstrap_path(runtime_root: &Path, run_id: Uuid) -> PathBuf {
    runtime_root
        .join("runs")
        .join(format!("{run_id}.bootstrap.json"))
}

/// Create the private runtime tree a worker discovers itself through.
pub fn prepare_runtime_root(runtime_root: &Path, uid: u32) -> Result<(), WorkerProtocolError> {
    for directory in [
        runtime_root.to_path_buf(),
        runtime_root.join("runs"),
        runtime_root.join("locks"),
        runtime_root.join("locks").join("startup"),
    ] {
        if fs::symlink_metadata(&directory).is_err() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .map_err(|_| WorkerProtocolError::Io)?;
        }
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
    }
    Ok(())
}

/// Everything a Run start decides about its worker, kept free of Run-record
/// types so worker discovery does not depend on the manifest module.
#[derive(Clone, Debug)]
pub struct RunWorkerStart {
    pub workspace_id: String,
    pub run_id: Uuid,
    pub run_generation: u64,
    pub ledger_root: PathBuf,
    pub dolgorae_version: String,
    pub mutation_protocol_version: u32,
    pub control_socket_epoch: u64,
    pub profile: String,
    pub session: Option<WorkerSessionBootstrap>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartedRun {
    pub run_id: Uuid,
    pub socket_path: PathBuf,
    pub pid: u32,
}

/// The kernel-owned boot-session identity qualifying every worker record.
///
/// This deliberately does not derive identity from Dolgorae's mutable runtime
/// tree: deleting or recreating `/tmp/dolgorae-<uid>` cannot make a record from
/// another boot appear current.
pub fn boot_session_uuid(uid: u32) -> Result<Uuid, WorkerProtocolError> {
    if DarwinSystem.current_uid() != uid {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let value = DarwinSystem
        .boot_session_uuid()
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    Uuid::parse_str(&value).map_err(|_| WorkerProtocolError::InvalidIdentity)
}

/// Start a worker after the caller acquired slot zero under the Profile
/// lifecycle fence. The range is released only after the child has published
/// its runtime record and `Bound` handoff.
#[cfg(target_os = "macos")]
pub fn start_run_worker_with_startup_range(
    state_root: &Path,
    start: &RunWorkerStart,
    startup_range: StartupLockFile,
) -> Result<StartedRun, WorkerProtocolError> {
    start_run_worker_inner(state_root, start, startup_range)
}

#[cfg(target_os = "macos")]
fn start_run_worker_inner(
    state_root: &Path,
    start: &RunWorkerStart,
    startup_range: StartupLockFile,
) -> Result<StartedRun, WorkerProtocolError> {
    let uid = DarwinSystem.current_uid();
    let runtime = runtime_root(state_root);
    prepare_runtime_root(&runtime, uid)?;
    let executable = std::env::current_exe().map_err(|_| WorkerProtocolError::Io)?;
    let executable = DarwinSystem
        .realpath(&executable)
        .map_err(|_| WorkerProtocolError::Io)?;
    let bootstrap = WorkerBootstrap {
        schema_version: 1,
        workspace_id: start.workspace_id.clone(),
        run_id: start.run_id,
        run_generation: start.run_generation,
        boot_uuid: boot_session_uuid(uid)?,
        executable_sha256: file_sha256(&executable)?,
        executable_path_sha256: HEXLOWER
            .encode(Sha256::digest(executable.as_os_str().as_encoded_bytes()).as_slice()),
        dolgorae_version: start.dolgorae_version.clone(),
        mutation_protocol_version: start.mutation_protocol_version,
        control_socket_epoch: start.control_socket_epoch,
        profile: start.profile.clone(),
        state_root: state_root.to_path_buf(),
        ledger_root: start.ledger_root.clone(),
        runtime_record_path: runtime_record_path(&runtime, start.run_id)?,
        startup_lock_path: startup_lock_path(&runtime, start.run_id),
        session: start.session.clone(),
    };
    let path = bootstrap_path(&runtime, start.run_id);
    let _ = fs::remove_file(&path);
    write_worker_bootstrap(&path, &bootstrap, uid)?;
    let started = spawn_hidden_worker_inner(&executable, &path, startup_range)?;
    Ok(StartedRun {
        run_id: start.run_id,
        socket_path: started.record.socket_path.clone(),
        pid: started.record.identity.pid,
    })
}

/// Send one control request to a Run's worker and read its single reply.
///
/// The caller never names the socket: identity comes from the durable runtime
/// record, so a stale pathname cannot redirect a control request.
///
/// `credential` is the caller's already-open Controller carrier descriptor. It
/// is handed to the worker with `SCM_RIGHTS`, never as a path, so the worker
/// authorizes the same open file the CLI checked rather than whatever the
/// pathname resolves to by the time the request lands.
pub fn call_run_worker(
    state_root: &Path,
    run_id: Uuid,
    uid: u32,
    credential: Option<RawFd>,
    build: impl FnOnce(WorkerIdentity) -> ControlRequestV1,
) -> Result<ControlResponseV1, WorkerProtocolError> {
    let runtime = runtime_root(state_root);
    let record = read_runtime_record(&runtime_record_path(&runtime, run_id)?, uid)?;
    let observed = record.observe_socket()?;
    if observed != record.socket_identity {
        return Err(WorkerProtocolError::SocketIdentityMismatch);
    }
    let request = build(record.identity.clone());
    // SPEC-004: an upgrade never silently mixes CLI and worker versions inside
    // one Run generation.  The worker's own identity check cannot catch this,
    // because the CLI addresses it with the identity the worker published; the
    // comparison that can is between the record's build and this process's.
    // Frozen control v1 is deliberately exempt, so `hello`, `status`, and
    // `shutdown` still reach a worker from another build.
    let mut request = request;
    if !request.frozen_control_v1() {
        let current = current_dolgorae_build()?;
        record.validate_ordinary_peer(&current)?;
        request.declare_caller(current);
    }
    let mut stream =
        UnixStream::connect(&record.socket_path).map_err(|_| WorkerProtocolError::Io)?;
    stream
        .set_read_timeout(Some(CONTROL_CALL_TIMEOUT))
        .map_err(|_| WorkerProtocolError::Io)?;
    stream
        .set_write_timeout(Some(CONTROL_CALL_TIMEOUT))
        .map_err(|_| WorkerProtocolError::Io)?;
    write_control_request(&mut stream, &request, credential)?;
    let reader_stream = stream.try_clone().map_err(|_| WorkerProtocolError::Io)?;
    let mut reader = std::io::BufReader::new(reader_stream);
    read_frame(&mut reader)
}

/// Write one control request, attaching `credential` to the frame's first byte.
///
/// The wire stays exactly one newline-delimited JSON frame; only the transport
/// of its first byte differs, so a caller that presents no credential is
/// byte-identical to the frozen control-v1 request it always was.
pub fn write_control_request(
    stream: &mut UnixStream,
    request: &ControlRequestV1,
    credential: Option<RawFd>,
) -> Result<(), WorkerProtocolError> {
    let Some(descriptor) = credential else {
        return write_frame(stream, request);
    };
    let bytes = serde_json::to_vec(request).map_err(|_| WorkerProtocolError::MalformedFrame)?;
    if bytes.len() > MAX_CLI_WORKER_FRAME_BYTES {
        return Err(frame_too_large(bytes.len()));
    }
    let (first, rest) = bytes
        .split_first()
        .ok_or(WorkerProtocolError::MalformedFrame)?;
    DarwinSystem
        .send_byte_with_optional_fd(stream, *first, Some(descriptor))
        .map_err(|_| WorkerProtocolError::Io)?;
    stream
        .write_all(rest)
        .map_err(|_| WorkerProtocolError::Io)?;
    stream
        .write_all(b"\n")
        .map_err(|_| WorkerProtocolError::Io)?;
    stream.flush().map_err(|_| WorkerProtocolError::Io)
}

fn file_sha256(path: &Path) -> Result<String, WorkerProtocolError> {
    let mut file = File::open(path).map_err(|_| WorkerProtocolError::Io)?;
    file_sha256_reader(&mut file)
}

fn file_sha256_reader(file: &mut File) -> Result<String, WorkerProtocolError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read =
            std::io::Read::read(&mut *file, &mut buffer).map_err(|_| WorkerProtocolError::Io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(HEXLOWER.encode(hasher.finalize().as_slice()))
}

pub fn write_runtime_record(
    path: &Path,
    record: &WorkerRuntimeRecord,
) -> Result<(), WorkerProtocolError> {
    record.validate()?;
    let parent = path
        .parent()
        .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| WorkerProtocolError::Io)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != record.identity.uid
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    let bytes =
        serde_json::to_vec(record).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    let temporary = parent.join(format!(".dolgorae-worker-{}", Uuid::now_v7()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|_| WorkerProtocolError::Io)?;
    file.write_all(&bytes)
        .map_err(|_| WorkerProtocolError::Io)?;
    file.write_all(b"\n").map_err(|_| WorkerProtocolError::Io)?;
    file.sync_all().map_err(|_| WorkerProtocolError::Io)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
        .map_err(|_| WorkerProtocolError::Io)?;
    if fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(WorkerProtocolError::Io);
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| WorkerProtocolError::Io)
}

pub fn read_runtime_record(
    path: &Path,
    uid: u32,
) -> Result<WorkerRuntimeRecord, WorkerProtocolError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > 64 * 1024
    {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    let bytes = fs::read(path).map_err(|_| WorkerProtocolError::Io)?;
    let record: WorkerRuntimeRecord =
        serde_json::from_slice(&bytes).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    record.validate()?;
    Ok(record)
}

/// Remove one stale worker locator only after the recorded generation and its
/// complete process scope are proved absent and the socket pathname still
/// names the exact recorded inode. A mismatch leaves every pathname intact.
pub fn remove_verified_absent_runtime(
    runtime_record: &Path,
    uid: u32,
) -> Result<(), WorkerProtocolError> {
    let record = read_runtime_record(runtime_record, uid)?;
    stop_orphaned_dedicated_server(&record)?;
    prove_worker_generation_absent(&record)?;
    match fs::symlink_metadata(&record.socket_path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket()
                || metadata.uid() != uid
                || metadata.dev() != record.socket_identity.device
                || metadata.ino() != record.socket_identity.inode
            {
                return Err(WorkerProtocolError::SocketIdentityMismatch);
            }
            fs::remove_file(&record.socket_path).map_err(|_| WorkerProtocolError::Io)?;
            File::open(
                record
                    .socket_path
                    .parent()
                    .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?,
            )
            .and_then(|directory| directory.sync_all())
            .map_err(|_| WorkerProtocolError::Io)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(WorkerProtocolError::SocketIdentityMismatch),
    }
    fs::remove_file(runtime_record).map_err(|_| WorkerProtocolError::Io)?;
    File::open(
        runtime_record
            .parent()
            .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?,
    )
    .and_then(|directory| directory.sync_all())
    .map_err(|_| WorkerProtocolError::Io)
}

/// Retire a Dedicated Run Server that survived its owning worker. The worker
/// must first be proved absent, and every signal is guarded by the recorded
/// four-verdict identity plus a complete same-scope census. A mismatched or
/// unreadable generation remains untouched and blocks replacement.
fn stop_orphaned_dedicated_server(record: &WorkerRuntimeRecord) -> Result<(), WorkerProtocolError> {
    let Some(server) = record.dedicated_server_identity.as_ref() else {
        return Ok(());
    };
    match classify_dedicated_server_identity(server) {
        ProcessIdentityVerdict::Absent => return Ok(()),
        ProcessIdentityVerdict::Match if dedicated_group_safe_to_signal(server) => {
            prove_group_empty(record, false, true)?;
        }
        _ => return Err(WorkerProtocolError::InvalidIdentity),
    }

    let _ = DarwinSystem.signal_process_group(server.process_group_id, libc::SIGTERM);
    let term_deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .unwrap_or_else(Instant::now);
    while Instant::now() < term_deadline {
        if recorded_dedicated_group_stopped(server) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if !recorded_dedicated_group_stopped(server) {
        if classify_dedicated_server_identity(server) != ProcessIdentityVerdict::Match
            || !dedicated_group_safe_to_signal(server)
        {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        let _ = DarwinSystem.signal_process_group(server.process_group_id, libc::SIGKILL);
    }

    let total_deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .unwrap_or_else(Instant::now);
    let mut empty_samples = 0_u8;
    while Instant::now() < total_deadline {
        if recorded_dedicated_group_stopped(server) {
            empty_samples = empty_samples.saturating_add(1);
            if empty_samples == 5 {
                return Ok(());
            }
        } else {
            empty_samples = 0;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(WorkerProtocolError::InvalidIdentity)
}

fn recorded_dedicated_group_stopped(server: &DedicatedServerIdentity) -> bool {
    classify_dedicated_server_identity(server) == ProcessIdentityVerdict::Absent
        && dedicated_group_has_only_expected_leader(server, false).is_ok_and(|empty| empty)
}

pub fn write_frame<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), WorkerProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| WorkerProtocolError::MalformedFrame)?;
    if bytes.len() > MAX_CLI_WORKER_FRAME_BYTES {
        return Err(frame_too_large(bytes.len()));
    }
    writer
        .write_all(&bytes)
        .map_err(|_| WorkerProtocolError::Io)?;
    writer
        .write_all(b"\n")
        .map_err(|_| WorkerProtocolError::Io)?;
    writer.flush().map_err(|_| WorkerProtocolError::Io)
}

pub fn stream_events_after<C: LedgerClock + 'static, F: FaultInjector + 'static, W: Write>(
    ledger: &Ledger<C, F>,
    after: u64,
    projection: EventProjection,
    writer: &mut W,
) -> Result<usize, WorkerProtocolError> {
    let deliveries = ledger
        .events_after(after, projection, true)
        .map_err(|_| WorkerProtocolError::LedgerReplay)?;
    for delivery in &deliveries {
        write_frame(writer, delivery)?;
    }
    let head = ledger
        .projection()
        .map_err(|_| WorkerProtocolError::LedgerReplay)?
        .ledger_head
        .sequence;
    write_frame(
        writer,
        &EventStreamEnd {
            schema_version: 1,
            kind: "event_stream_end".to_owned(),
            next_cursor: head.to_string(),
        },
    )?;
    Ok(deliveries.len())
}

/// The frame-bound refusal, carrying the size it actually observed.
fn frame_too_large(observed_bytes: usize) -> WorkerProtocolError {
    WorkerProtocolError::FrameTooLarge {
        observed_bytes: observed_bytes as u64,
    }
}

pub fn read_frame<R: BufRead, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<T, WorkerProtocolError> {
    let mut frame = Vec::new();
    let mut oversized = false;
    // Counted even past the bound, because the refusal has to say how large
    // the frame actually was, not merely that it was too large.
    let mut observed = 0_usize;
    loop {
        let available = reader.fill_buf().map_err(|_| WorkerProtocolError::Io)?;
        if available.is_empty() {
            return Err(WorkerProtocolError::MalformedFrame);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let payload = newline.map_or(available, |index| &available[..index]);
        observed = observed.saturating_add(payload.len());
        if !oversized {
            if frame.len().saturating_add(payload.len()) > MAX_CLI_WORKER_FRAME_BYTES {
                oversized = true;
            } else {
                frame.extend_from_slice(payload);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if oversized {
        return Err(frame_too_large(observed));
    }
    if frame.is_empty() || frame.contains(&0) {
        return Err(WorkerProtocolError::MalformedFrame);
    }
    serde_json::from_slice(&frame).map_err(|_| WorkerProtocolError::MalformedFrame)
}

fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut decoded = [0_u8; 32];
    HEXLOWER.decode_mut(value.as_bytes(), &mut decoded).ok()?;
    Some(decoded)
}

fn prepare_socket_root(uid: u32, socket_parent: &Path) -> Result<(), WorkerProtocolError> {
    let expected_root = PathBuf::from(format!("/tmp/dolgorae-{uid}"));
    if socket_parent != expected_root.join("s") && socket_parent != expected_root.join("c") {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    for directory in [&expected_root, socket_parent] {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(WorkerProtocolError::Io),
        }
        let metadata = fs::symlink_metadata(directory).map_err(|_| WorkerProtocolError::Io)?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
    }
    Ok(())
}

/// The contended range of a Run's startup lock, as a signed `fcntl` offset.
fn startup_range_offset() -> Result<i64, WorkerProtocolError> {
    i64::try_from(slot_offset(0)?).map_err(|_| WorkerProtocolError::InvalidOwnerRecord)
}

fn slot_offset(slot: u8) -> Result<u64, WorkerProtocolError> {
    match slot {
        0 => Ok(0),
        1 => Ok(STARTUP_SLOT_BYTES as u64),
        _ => Err(WorkerProtocolError::InvalidOwnerRecord),
    }
}

/// The live Run a control caller may reach, if this worker owns one.
#[derive(Clone, Default)]
pub struct RunHandles {
    pub session: Option<Arc<WorkerSession>>,
    pub ledger: Option<Arc<Mutex<Ledger>>>,
}

impl RunHandles {
    fn session(&self) -> Option<&Arc<WorkerSession>> {
        self.session.as_ref()
    }

    fn ledger(&self) -> Option<&Arc<Mutex<Ledger>>> {
        self.ledger.as_ref()
    }
}

/// A Run whose worker has bound its socket but has not yet attached the Run.
///
/// This is the replay window, not a missing Run: the caller may come back, so
/// it is reported under the registered, retryable `RUN_BUSY` with startup as
/// the owner rather than an unregistered code of its own.
fn run_not_started(facts: &RunFacts, missing: &str) -> ControlResponseV1 {
    run_busy(facts, "startup", &format!("worker owns no {missing} yet"))
}

/// The registered busy refusal, naming which owner is holding the Run.
fn run_busy(facts: &RunFacts, owner_kind: &str, message: &str) -> ControlResponseV1 {
    ControlResponseV1::Failed {
        code: "RUN_BUSY".to_owned(),
        message: message.to_owned(),
        retryable: true,
        details: serde_json::json!({"run_id": facts.run_id, "owner_kind": owner_kind}),
    }
}

fn failed(error: &TurnError, context: &TurnFailureContext) -> ControlResponseV1 {
    let machine = error.clone().into_machine_error(context);
    ControlResponseV1::Failed {
        code: machine.code,
        message: machine.message,
        retryable: machine.retryable,
        details: machine.details,
    }
}

fn serve_control_caller(
    mut stream: UnixStream,
    hello: &WorkerHello,
    facts: &RunFacts,
    state: &Arc<Mutex<WorkerControlState>>,
    stopping: &AtomicBool,
    run: &RunHandles,
    authority: &RunControllerAuthority,
) -> Result<(), WorkerProtocolError> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| WorkerProtocolError::Io)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|_| WorkerProtocolError::Io)?;
    // The credential descriptor rides the request frame's first byte, so it is
    // taken with a one-byte `recvmsg` before any buffered read can consume the
    // boundary the control message is attached to and discard the descriptor.
    let (leading, received) = DarwinSystem
        .receive_byte_with_optional_fd(&stream)
        .map_err(|_| WorkerProtocolError::Io)?;
    let reader_stream = stream.try_clone().map_err(|_| WorkerProtocolError::Io)?;
    let mut reader = std::io::BufReader::new(std::io::Read::chain(
        std::io::Cursor::new([leading]),
        reader_stream,
    ));
    let request: ControlRequestV1 = read_frame(&mut reader)?;
    // SPEC-004: "upgrade does not silently mix CLI and worker versions within
    // one run generation."  Identity alone cannot prove that — the caller
    // addresses the worker with the identity the worker published — so a
    // non-frozen request must also declare the build that composed it, and the
    // worker refuses one that is not its own.  Frozen control v1 stays exempt.
    let peer_matches = request.frozen_control_v1()
        || request.caller().is_some_and(|caller| {
            caller.version == hello.dolgorae_version
                && caller.mutation_protocol_version == hello.mutation_protocol_version
                && caller.binary_sha256 == hello.binary_sha256
        });
    if request.expected() != &hello.identity || !peer_matches {
        return write_frame(
            &mut stream,
            &ControlResponseV1::Rejected {
                code: "DOLGORAE_PROTOCOL_MISMATCH".to_owned(),
            },
        );
    }
    let response = match request {
        ControlRequestV1::Hello { .. } => ControlResponseV1::Hello {
            hello: hello.clone(),
        },
        // Frozen control v1: exactly the v1 members, so a caller from an older
        // or newer build parses this answer byte for byte.
        ControlRequestV1::Status { .. } => {
            let snapshot = state.lock().map_err(|_| WorkerProtocolError::Io)?.clone();
            ControlResponseV1::Status {
                identity: hello.identity.clone(),
                lifecycle: snapshot.lifecycle,
                active_turn: snapshot.active_turn,
                last_terminal: None,
            }
        }
        // The same state, plus the terminal the drain observed, for a caller
        // that proved it shares this worker's build.
        ControlRequestV1::RunStatus { .. } => {
            let snapshot = state.lock().map_err(|_| WorkerProtocolError::Io)?.clone();
            ControlResponseV1::Status {
                identity: hello.identity.clone(),
                lifecycle: snapshot.lifecycle,
                active_turn: snapshot.active_turn,
                last_terminal: run.session().and_then(|session| session.last_terminal()),
            }
        }
        ControlRequestV1::Shutdown { .. } => {
            // ADR-011: shutdown interrupts an active Turn and waits for its
            // terminal history rather than declaring the Run idle.  What is
            // reported is what was observed — a Run whose Turn outlived the
            // budget has just recorded `outcome_unknown` and says so.
            let terminal_confirmed = run
                .session()
                .is_none_or(|session| session.settle_before_shutdown());
            write_frame(
                &mut stream,
                &ControlResponseV1::Shutdown {
                    identity: hello.identity.clone(),
                    terminal_confirmed,
                },
            )?;
            stopping.store(true, Ordering::Release);
            return Ok(());
        }
        ControlRequestV1::Events {
            after,
            projection,
            limit,
            ..
        } => serve_events(facts, run, after, projection, limit),
        other => {
            // A mutating request whose descriptor is absent, or is not a
            // same-uid private regular file within bounds, is refused before
            // the Run is touched at all. An observer's stray descriptor is
            // closed here instead of being trusted.
            let mutating = other.requires_controller();
            let credential = mutating
                .then(|| received.and_then(|fd| CredentialCarrier::from_received_fd(fd).ok()))
                .flatten();
            if mutating && credential.is_none() {
                return write_frame(
                    &mut stream,
                    &controller_refused(facts.run_id, other.operation_name()),
                );
            }
            serve_run_operation(other, facts, run, authority, credential)?
        }
    };
    write_frame(&mut stream, &response)
}

/// The non-oracular refusal a missing, unreadable, wrong, or superseded
/// Controller credential all share, so a caller cannot tell which it was.
fn controller_refused(run_id: Uuid, operation: &str) -> ControlResponseV1 {
    ControlResponseV1::Failed {
        code: "CONTROLLER_MISMATCH".to_owned(),
        message: format!("controller credential does not authorize {operation}"),
        retryable: false,
        details: serde_json::json!({"run_id": run_id, "operation": operation}),
    }
}

#[cfg(test)]
fn revalidate_controller(
    authority: &RunControllerAuthority,
    operation: &str,
    credential: Option<&CredentialCarrier>,
) -> Option<ControlResponseV1> {
    authority
        .authorize_mutation(operation, credential, None)
        .err()
        .map(refusal_from)
}

/// The durable authority already produced a contract-shaped machine error, so
/// it travels whole rather than being flattened to a code.
fn refusal_from(error: MachineError) -> ControlResponseV1 {
    ControlResponseV1::Failed {
        code: error.code,
        message: error.message,
        retryable: error.retryable,
        details: error.details,
    }
}

/// Observer reads take the ledger lock only for the length of one read, so a
/// Turn that is still running never blocks them.
fn serve_events(
    facts: &RunFacts,
    run: &RunHandles,
    after: u64,
    projection: EventProjection,
    limit: usize,
) -> ControlResponseV1 {
    let Some(ledger) = run.ledger() else {
        return run_not_started(facts, "durable ledger");
    };
    let Ok(ledger) = ledger.lock() else {
        return ControlResponseV1::Failed {
            code: "INTERNAL_ERROR".to_owned(),
            message: "ledger lock is poisoned".to_owned(),
            retryable: false,
            details: serde_json::json!({"invariant": "run ledger lock is poisoned"}),
        };
    };
    let bounded = limit.clamp(1, MAX_CONTROL_EVENT_PAGE);
    let head = match ledger.projection() {
        Ok(state) => state.ledger_head.sequence,
        Err(error) => return ledger_read_failed(&error.to_string()),
    };
    // docs/specs/README.md: a cursor "beyond the authoritative Run ledger head" is
    // `EVENT_CURSOR_INVALID`, not a malformed argument.  The head is the
    // worker's own, so the refusal names both cursors the caller needs.
    if after > head {
        return event_cursor_invalid(facts.run_id, &after.to_string(), &head.to_string());
    }
    match ledger.events_after(after, projection, true) {
        Ok(mut deliveries) => {
            deliveries.truncate(bounded);
            let next_cursor = deliveries.last().map_or_else(
                || head.to_string(),
                |delivery| delivery.record.cursor.clone(),
            );
            ControlResponseV1::Events {
                deliveries,
                next_cursor,
                head_cursor: head.to_string(),
            }
        }
        Err(error) => ledger_read_failed(&error.to_string()),
    }
}

/// The registered refusal for a noncanonical or beyond-head event cursor.
#[must_use]
pub fn event_cursor_invalid(
    run_id: Uuid,
    requested_cursor: &str,
    head_cursor: &str,
) -> ControlResponseV1 {
    ControlResponseV1::Failed {
        code: "EVENT_CURSOR_INVALID".to_owned(),
        message: "event cursor is noncanonical or beyond the run ledger head".to_owned(),
        retryable: false,
        details: serde_json::json!({
            "run_id": run_id,
            "requested_cursor": requested_cursor,
            "head_cursor": head_cursor,
        }),
    }
}

/// A durable ledger that could not be read is an integrity fact about the Run,
/// not a fact about the caller's arguments.
fn ledger_read_failed(reason: &str) -> ControlResponseV1 {
    ControlResponseV1::Failed {
        code: "INTERNAL_ERROR".to_owned(),
        message: "run ledger could not be read".to_owned(),
        retryable: false,
        details: serde_json::json!({"invariant": reason}),
    }
}

/// Rejoin one addressed Turn, falling back to the Run's durable ledger.
///
/// A live Run remembers only its most recent terminals, because a worker's
/// control state is bounded process memory.  A caller that comes back for an
/// older Turn — after a long Submit, or across a generation this worker
/// replayed rather than ran — would otherwise be told the Run never had a Turn
/// whose outcome is sitting in the ledger.  So `TURN_NOT_FOUND` is put to the
/// durable record before it is put to the caller.
///
/// Only that one refusal is reconsidered.  Every other answer is the live
/// Run's own and outranks the ledger: a Turn that is still running has no
/// terminal to find, and a fault the Run reported is not improved by
/// answering a stale success instead.
fn rejoin_turn(
    session: &WorkerSession,
    run: &RunHandles,
    turn_id: &str,
    budget: Option<Duration>,
) -> ControlResponseV1 {
    let answer = session.wait(turn_id, budget);
    if !matches!(&answer, ControlResponseV1::Failed { code, .. } if code == "TURN_NOT_FOUND") {
        return answer;
    }
    let Some(ledger) = run.ledger() else {
        return answer;
    };
    let Ok(ledger) = ledger.lock() else {
        return answer;
    };
    recorded_terminal(&ledger, turn_id).map_or(answer, terminal_response)
}

/// One caller's own timeout, bounded so a malformed value cannot outlast the
/// wait ceiling or overflow the clock.
fn caller_budget(timeout_ms: Option<u64>) -> Option<Duration> {
    timeout_ms.map(|milliseconds| Duration::from_millis(milliseconds).min(MAX_TURN_WAIT_TIMEOUT))
}

/// Hand one Run verb to the Run.
///
/// A mutation is queued for the worker's drain, which revalidates the carried
/// Controller credential immediately before the effect; an observer read of Run
/// state answers from what the drain has already published.  Neither path holds
/// the Run still, so a Turn that is running never blocks the next verb.
fn serve_run_operation(
    request: ControlRequestV1,
    facts: &RunFacts,
    run: &RunHandles,
    authority: &RunControllerAuthority,
    credential: Option<CredentialCarrier>,
) -> Result<ControlResponseV1, WorkerProtocolError> {
    let operation = request.operation_name();
    let mutating = request.requires_controller();
    let external_engagement = request.external_engagement();
    let Some(session) = run.session().map(Arc::clone) else {
        // A Run with no session still proves authority first, so an
        // unauthorized caller never gets a cheaper answer than an authorized
        // one for the same request.
        if mutating {
            let authorization =
                authority.authorize_mutation(operation, credential.as_ref(), external_engagement);
            if let Err(error) = authorization {
                return Ok(refusal_from(error));
            }
        }
        // A reset fence answered from here would be a guess: the worker is
        // still replaying and has no authoritative Turn state yet.  The
        // resetting operator gets the retryable startup answer and comes back
        // — but only after proving the same durable fence it would have to
        // prove with a session attached, so an unauthorized caller never gets
        // a cheaper answer than an authorized one here either.
        // The worker owns no session yet, so the Run is exactly what its
        // lifecycle says it is here: still starting.
        if let ControlRequestV1::ResetFence { confirmation, .. } = request
            && let Err(error) = authority.reset_fence(confirmation, "starting")
        {
            return Ok(refusal_from(error));
        }
        return Ok(run_not_started(facts, "app-server session"));
    };
    if let ControlRequestV1::Wait {
        turn_id,
        timeout_ms,
        ..
    } = &request
    {
        return Ok(rejoin_turn(
            &session,
            run,
            turn_id,
            caller_budget(*timeout_ms),
        ));
    }
    if let ControlRequestV1::ResetFence { confirmation, .. } = request {
        return Ok(session.reset_fence(operation, confirmation));
    }
    let Some(credential) = credential else {
        return Ok(controller_refused(facts.run_id, operation));
    };
    Ok(match request {
        ControlRequestV1::Send {
            request,
            timeout_ms,
            ..
        } => session.submit(
            operation,
            credential,
            request,
            DeliveryMode::Send,
            caller_budget(timeout_ms),
        ),
        ControlRequestV1::Submit { request, .. } => {
            session.submit(operation, credential, request, DeliveryMode::Submit, None)
        }
        ControlRequestV1::ExternalSubmit {
            engagement_id,
            request,
            ..
        } => session.external_submit(credential, engagement_id, request),
        ControlRequestV1::Respond {
            request_id,
            idempotency_key,
            response,
            ..
        } => session.respond(operation, credential, request_id, idempotency_key, response),
        ControlRequestV1::Interrupt { .. } => session.interrupt(operation, credential),
        ControlRequestV1::ExternalInterrupt { engagement_id, .. } => {
            session.external_interrupt(credential, engagement_id)
        }
        ControlRequestV1::Pause { interrupt, .. } => {
            session.pause(operation, credential, interrupt)
        }
        ControlRequestV1::Resume { .. } => session.resume(operation, credential),
        ControlRequestV1::Reconcile { .. } => session.reconcile(operation, credential),
        ControlRequestV1::SettleLifecycleTimeout { close, .. } => {
            session.settle_lifecycle_timeout(operation, credential, close)
        }
        ControlRequestV1::Close { interrupt, .. } => {
            session.close(operation, credential, interrupt)
        }
        ControlRequestV1::ExternalClose {
            engagement_id,
            interrupt,
            ..
        } => session.external_close(credential, engagement_id, interrupt),
        ControlRequestV1::SetWriterAccess {
            write,
            writer_generation,
            transaction_id,
            ..
        } => session.set_writer_access(
            operation,
            credential,
            write,
            writer_generation,
            transaction_id,
        ),
        ControlRequestV1::ExternalSetWriterAccess {
            engagement_id,
            write,
            writer_generation,
            transaction_id,
            ..
        } => session.external_set_writer_access(
            credential,
            engagement_id,
            write,
            writer_generation,
            transaction_id,
        ),
        ControlRequestV1::Hello { .. }
        | ControlRequestV1::Status { .. }
        | ControlRequestV1::RunStatus { .. }
        | ControlRequestV1::Shutdown { .. }
        | ControlRequestV1::Wait { .. }
        | ControlRequestV1::ResetFence { .. }
        | ControlRequestV1::Events { .. } => {
            return Err(WorkerProtocolError::ProtocolMismatch {
                expected_protocol: WORKER_PROTOCOL_VERSION,
                actual_protocol: WORKER_PROTOCOL_VERSION,
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};
    #[cfg(target_os = "macos")]
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    use std::time::Instant;

    fn identity() -> WorkerIdentity {
        WorkerIdentity {
            workspace_id: "11".repeat(32),
            run_id: Uuid::now_v7(),
            run_generation: 3,
            boot_uuid: Uuid::parse_str("2e349290-1744-4fc3-bb62-9cbf9f5859c0").unwrap(),
            pid: 42,
            process_group_id: 42,
            session_id: 42,
            uid: 501,
            start_tvsec: 1,
            start_tvusec: 0,
            executable_path: PathBuf::from("/usr/bin/true"),
            executable_device: 1,
            executable_inode: 1,
            executable_sha256: "22".repeat(32),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn held_startup_range_rejects_path_mismatch_and_releases_after_spawn_failure() {
        let state_root =
            std::env::temp_dir().join(format!("dolgorae-held-startup-{}", Uuid::now_v7()));
        fs::create_dir(&state_root).unwrap();
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = runtime_root(&state_root);
        let uid = DarwinSystem.current_uid();
        prepare_runtime_root(&runtime, uid).unwrap();
        let run_id = Uuid::now_v7();
        let expected_lock_path = startup_lock_path(&runtime, run_id);
        let bootstrap = WorkerBootstrap {
            schema_version: 1,
            workspace_id: "1".repeat(64),
            run_id,
            run_generation: 1,
            boot_uuid: boot_session_uuid(uid).unwrap(),
            executable_sha256: "2".repeat(64),
            executable_path_sha256: "3".repeat(64),
            dolgorae_version: env!("CARGO_PKG_VERSION").to_owned(),
            mutation_protocol_version: WORKER_PROTOCOL_VERSION,
            control_socket_epoch: 1,
            profile: "default".to_owned(),
            state_root: state_root.clone(),
            ledger_root: state_root.join("runs").join(run_id.to_string()),
            runtime_record_path: runtime_record_path(&runtime, run_id).unwrap(),
            startup_lock_path: expected_lock_path.clone(),
            session: None,
        };
        let bootstrap_path = bootstrap_path(&runtime, run_id);
        write_worker_bootstrap(&bootstrap_path, &bootstrap, uid).unwrap();

        let other_lock_path = startup_lock_path(&runtime, Uuid::now_v7());
        let other_lock = StartupLockFile::open(&other_lock_path, uid).unwrap();
        other_lock
            .hold_startup_range(Duration::from_secs(1))
            .unwrap();
        other_lock
            .write_owner(
                &current_startup_owner(&bootstrap.workspace_id, run_id, bootstrap.run_generation)
                    .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            spawn_hidden_worker_inner(
                &std::env::current_exe().unwrap(),
                &bootstrap_path,
                other_lock
            ),
            Err(WorkerProtocolError::InvalidRuntimeRecord)
        ));
        assert_eq!(
            StartupLockFile::open(&other_lock_path, uid)
                .unwrap()
                .read_owner(0)
                .unwrap(),
            None
        );

        let expected_lock = StartupLockFile::open(&expected_lock_path, uid).unwrap();
        expected_lock
            .hold_startup_range(Duration::from_secs(1))
            .unwrap();
        let owner =
            current_startup_owner(&bootstrap.workspace_id, run_id, bootstrap.run_generation)
                .unwrap();
        assert_eq!(owner.slot, 0);
        assert_eq!(owner.identity.run_id, run_id);
        assert_eq!(owner.identity.pid, std::process::id());
        owner.identity.validate().unwrap();
        expected_lock.write_owner(&owner).unwrap();
        assert!(matches!(
            spawn_hidden_worker_inner(
                Path::new("/definitely/missing/dolgorae-worker"),
                &bootstrap_path,
                expected_lock,
            ),
            Err(WorkerProtocolError::Io)
        ));
        assert_eq!(
            StartupLockFile::open(&expected_lock_path, uid)
                .unwrap()
                .read_owner(0)
                .unwrap(),
            None
        );
        let contender = StartupLockFile::open(&expected_lock_path, uid).unwrap();
        contender.hold_startup_range(Duration::ZERO).unwrap();
        contender.release_startup_range().unwrap();
        fs::remove_dir_all(state_root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn four_verdict_identity_matches_current_and_rejects_recorded_reuse() {
        let process = DarwinSystem.current_process().unwrap();
        let sample = DarwinSystem.bsd_process_identity(process.pid).unwrap();
        let executable_path = DarwinSystem
            .realpath(&std::env::current_exe().unwrap())
            .unwrap();
        let metadata = fs::metadata(&executable_path).unwrap();
        let identity = WorkerIdentity {
            workspace_id: "11".repeat(32),
            run_id: Uuid::now_v7(),
            run_generation: 1,
            boot_uuid: Uuid::parse_str(&DarwinSystem.boot_session_uuid().unwrap()).unwrap(),
            pid: process.pid,
            process_group_id: process.process_group_id,
            session_id: sample.session_id,
            uid: process.uid,
            start_tvsec: sample.start_tvsec,
            start_tvusec: sample.start_tvusec,
            executable_path: executable_path.clone(),
            executable_device: metadata.dev(),
            executable_inode: metadata.ino(),
            executable_sha256: file_sha256(&std::env::current_exe().unwrap()).unwrap(),
        };
        let mut record = WorkerRuntimeRecord {
            schema_version: 1,
            socket_path: worker_socket_path(identity.uid, &identity.workspace_id, identity.run_id)
                .unwrap(),
            identity,
            socket_identity: SocketIdentity {
                device: 1,
                inode: 1,
            },
            control_socket_epoch: 1,
            app_server_epoch: None,
            dedicated_server_identity: None,
            dolgorae_version: env!("CARGO_PKG_VERSION").to_owned(),
            mutation_protocol_version: WORKER_PROTOCOL_VERSION,
            binary_sha256: current_dolgorae_build().unwrap().binary_sha256,
        };
        assert_eq!(
            classify_worker_identity(&record),
            ProcessIdentityVerdict::Match
        );
        record.identity.start_tvusec = record.identity.start_tvusec.saturating_add(1);
        assert_eq!(
            classify_worker_identity(&record),
            ProcessIdentityVerdict::Mismatch
        );
        record.identity.start_tvusec = sample.start_tvusec;
        record.identity.executable_path = PathBuf::from("/missing/dolgorae-worker");
        assert_eq!(
            classify_worker_identity(&record),
            ProcessIdentityVerdict::Unverifiable
        );
        record.identity.executable_path = executable_path;
        record.identity.boot_uuid = Uuid::now_v7();
        assert_eq!(
            classify_worker_identity(&record),
            ProcessIdentityVerdict::Absent
        );
    }

    #[test]
    fn process_scope_does_not_trust_effective_uid_changes() {
        let identity = crate::darwin::BsdProcessIdentity {
            pid: 44,
            parent_pid: 43,
            uid: 0,
            process_group_id: 42,
            session_id: 42,
            start_tvsec: 1,
            start_tvusec: 0,
            zombie: false,
        };
        let parents = [(44, 43), (43, 42)].into_iter().collect();
        assert!(process_is_in_scope(&identity, 42, 42, 42, &parents));

        let detached = crate::darwin::BsdProcessIdentity {
            process_group_id: 99,
            session_id: 99,
            parent_pid: 1,
            ..identity
        };
        assert!(!process_is_in_scope(
            &detached,
            42,
            42,
            42,
            &[(44, 1)].into_iter().collect()
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dedicated_server_does_not_hide_worker_group_workload() {
        fn executable_identity(path: &Path) -> (PathBuf, u64, u64, String) {
            let path = DarwinSystem.realpath(path).unwrap();
            let metadata = fs::metadata(&path).unwrap();
            let digest = file_sha256(&path).unwrap();
            (path, metadata.dev(), metadata.ino(), digest)
        }

        let mut worker_command = Command::new("/bin/sh");
        worker_command
            .args(["-c", "sleep 30 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut worker = DarwinSystem.spawn_detached(&mut worker_command).unwrap();
        let mut server_command = Command::new("/bin/sleep");
        server_command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut server = DarwinSystem.spawn_detached(&mut server_command).unwrap();
        std::thread::sleep(Duration::from_millis(100));

        let worker_process = DarwinSystem.bsd_process_identity(worker.id()).unwrap();
        let server_process = DarwinSystem.bsd_process_identity(server.id()).unwrap();
        let (worker_path, worker_device, worker_inode, worker_digest) =
            executable_identity(Path::new("/bin/sh"));
        let (server_path, server_device, server_inode, server_digest) =
            executable_identity(Path::new("/bin/sleep"));
        let identity = WorkerIdentity {
            workspace_id: "11".repeat(32),
            run_id: Uuid::now_v7(),
            run_generation: 1,
            boot_uuid: Uuid::parse_str(&DarwinSystem.boot_session_uuid().unwrap()).unwrap(),
            pid: worker_process.pid,
            process_group_id: worker_process.process_group_id,
            session_id: worker_process.session_id,
            uid: worker_process.uid,
            start_tvsec: worker_process.start_tvsec,
            start_tvusec: worker_process.start_tvusec,
            executable_path: worker_path,
            executable_device: worker_device,
            executable_inode: worker_inode,
            executable_sha256: worker_digest,
        };
        let record = WorkerRuntimeRecord {
            schema_version: 1,
            socket_path: worker_socket_path(identity.uid, &identity.workspace_id, identity.run_id)
                .unwrap(),
            identity,
            socket_identity: SocketIdentity {
                device: 1,
                inode: 1,
            },
            control_socket_epoch: 1,
            app_server_epoch: Some(1),
            dedicated_server_identity: Some(DedicatedServerIdentity {
                pid: server_process.pid,
                process_group_id: server_process.process_group_id,
                session_id: server_process.session_id,
                uid: server_process.uid,
                start_tvsec: server_process.start_tvsec,
                start_tvusec: server_process.start_tvusec,
                executable_path: server_path,
                executable_device: server_device,
                executable_inode: server_inode,
                executable_sha256: server_digest,
            }),
            dolgorae_version: env!("CARGO_PKG_VERSION").to_owned(),
            mutation_protocol_version: WORKER_PROTOCOL_VERSION,
            binary_sha256: current_dolgorae_build().unwrap().binary_sha256,
        };

        assert_eq!(
            classify_worker_identity(&record),
            ProcessIdentityVerdict::Match
        );
        assert_eq!(
            classify_dedicated_server_identity(record.dedicated_server_identity.as_ref().unwrap()),
            ProcessIdentityVerdict::Match
        );
        assert_eq!(
            prove_worker_workload_absent(&record),
            Err(WorkerProtocolError::InvalidIdentity)
        );

        let _ = DarwinSystem.signal_process_group(worker.id(), libc::SIGKILL);
        let _ = DarwinSystem.signal_process_group(server.id(), libc::SIGKILL);
        let _ = worker.wait();
        let _ = server.wait();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_orphaned_dedicated_server_is_retired_only_after_worker_absence() {
        let mut worker_command = Command::new("/bin/sleep");
        worker_command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut worker = DarwinSystem.spawn_detached(&mut worker_command).unwrap();
        let mut server_command = Command::new("/bin/sleep");
        server_command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut server = DarwinSystem.spawn_detached(&mut server_command).unwrap();
        std::thread::sleep(Duration::from_millis(100));

        let worker_observed = DarwinSystem.bsd_process_identity(worker.id()).unwrap();
        let observed = DarwinSystem.bsd_process_identity(server.id()).unwrap();
        let executable_path = DarwinSystem.realpath(Path::new("/bin/sleep")).unwrap();
        let metadata = fs::metadata(&executable_path).unwrap();
        let server_identity = DedicatedServerIdentity {
            pid: observed.pid,
            process_group_id: observed.process_group_id,
            session_id: observed.session_id,
            uid: observed.uid,
            start_tvsec: observed.start_tvsec,
            start_tvusec: observed.start_tvusec,
            executable_path: executable_path.clone(),
            executable_device: metadata.dev(),
            executable_inode: metadata.ino(),
            executable_sha256: file_sha256(&executable_path).unwrap(),
        };
        let _ = DarwinSystem.signal_process_group(worker.id(), libc::SIGKILL);
        let _ = worker.wait();
        std::thread::sleep(Duration::from_millis(100));

        let current = DarwinSystem.current_process().unwrap();
        let workspace_id = "11".repeat(32);
        let run_id = Uuid::now_v7();
        let record = WorkerRuntimeRecord {
            schema_version: 1,
            socket_path: worker_socket_path(current.uid, &workspace_id, run_id).unwrap(),
            identity: WorkerIdentity {
                workspace_id,
                run_id,
                run_generation: 1,
                boot_uuid: Uuid::now_v7(),
                pid: worker_observed.pid,
                process_group_id: worker_observed.process_group_id,
                session_id: worker_observed.session_id,
                uid: worker_observed.uid,
                start_tvsec: worker_observed.start_tvsec,
                start_tvusec: worker_observed.start_tvusec,
                executable_path: executable_path.clone(),
                executable_device: metadata.dev(),
                executable_inode: metadata.ino(),
                executable_sha256: file_sha256(&executable_path).unwrap(),
            },
            socket_identity: SocketIdentity {
                device: 1,
                inode: 1,
            },
            control_socket_epoch: 1,
            app_server_epoch: Some(1),
            dedicated_server_identity: Some(server_identity.clone()),
            dolgorae_version: env!("CARGO_PKG_VERSION").to_owned(),
            mutation_protocol_version: WORKER_PROTOCOL_VERSION,
            binary_sha256: current_dolgorae_build().unwrap().binary_sha256,
        };

        assert_eq!(
            classify_worker_identity(&record),
            ProcessIdentityVerdict::Absent
        );
        stop_orphaned_dedicated_server(&record).unwrap();
        assert_eq!(
            classify_dedicated_server_identity(&server_identity),
            ProcessIdentityVerdict::Absent
        );
        let _ = server.wait();
    }

    fn reviewer_bootstrap() -> WorkerSessionBootstrap {
        WorkerSessionBootstrap {
            app_server_socket: PathBuf::from("/tmp/app-server.sock"),
            canonical_codex_home: "/tmp/codex-home".to_owned(),
            server_key: "a".repeat(64),
            server_epoch: 1,
            controller_id: Uuid::now_v7(),
            control_mode: "direct_interactive".to_owned(),
            fixed_model: "gpt-5".to_owned(),
            default_effort: "high".to_owned(),
            supported_efforts: vec!["high".to_owned()],
            cwd: PathBuf::from("/tmp/workspace"),
            developer_instructions: "review".to_owned(),
            sandbox: "read-only".to_owned(),
            approval_policy: "never".to_owned(),
            safety_policy: SessionSafetyPolicy::ReviewerReadOnly,
            artifact_root: PathBuf::from("/tmp/artifacts"),
            attach: SessionAttach::Start,
            transport_timeout_seconds: 60,
            dedicated_server: None,
        }
    }

    #[test]
    fn reviewer_bootstrap_requires_read_only_never_and_accepts_the_exact_pair() {
        let valid = reviewer_bootstrap();
        assert_eq!(valid.validate(), Ok(()));

        let mut writable = valid.clone();
        writable.sandbox = "workspace-write".to_owned();
        assert_eq!(
            writable.validate(),
            Err(WorkerProtocolError::InvalidRuntimeRecord)
        );

        let mut prompting = valid;
        prompting.approval_policy = "untrusted".to_owned();
        assert_eq!(
            prompting.validate(),
            Err(WorkerProtocolError::InvalidRuntimeRecord)
        );
    }

    #[test]
    fn the_bind_budget_is_separate_from_the_larger_replay_budget() {
        assert!(
            STARTUP_BOUND_TIMEOUT < STARTUP_READY_TIMEOUT,
            "replay must be allowed to outlast binding"
        );
        let (parent, child) = UnixStream::pair().unwrap();
        let record = WorkerRuntimeRecord {
            schema_version: 1,
            identity: identity(),
            socket_path: PathBuf::from("/tmp/dolgorae-501/s/x.sock"),
            socket_identity: SocketIdentity {
                device: 1,
                inode: 2,
            },
            control_socket_epoch: 1,
            app_server_epoch: None,
            dedicated_server_identity: None,
            dolgorae_version: "0.1.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "22".repeat(32),
        };
        let mut writer = child;
        write_frame(
            &mut writer,
            &StartupHandoff::Bound {
                record: record.clone(),
            },
        )
        .unwrap();
        let (mut reader, bound) = read_bound_handoff(parent).unwrap();
        assert_eq!(bound, record);
        assert_eq!(
            reader.get_ref().read_timeout().unwrap(),
            Some(STARTUP_READY_TIMEOUT),
            "the replay phase must not inherit the bind budget"
        );
        write_frame(
            &mut writer,
            &StartupHandoff::Ready {
                record: record.clone(),
            },
        )
        .unwrap();
        assert_eq!(read_ready_handoff(&mut reader).unwrap(), record);
    }

    #[test]
    fn a_worker_that_never_binds_fails_inside_the_bind_budget() {
        let (parent, _child) = UnixStream::pair().unwrap();
        let started = std::time::Instant::now();
        assert!(read_bound_handoff(parent).is_err());
        assert!(
            started.elapsed() < STARTUP_READY_TIMEOUT,
            "a stuck worker must not hold a caller for the replay budget"
        );
    }

    #[test]
    fn a_control_request_before_the_run_attaches_is_registered_and_retryable() {
        let hello = WorkerHello {
            schema_version: 1,
            identity: identity(),
            control_socket_epoch: 7,
            dolgorae_version: "0.1.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "22".repeat(32),
        };
        let facts = RunFacts {
            run_id: hello.identity.run_id,
            profile: "default".to_owned(),
        };
        let response = serve_run_operation(
            ControlRequestV1::Wait {
                caller: None,
                expected: hello.identity.clone(),
                turn_id: "turn-1".to_owned(),
                timeout_ms: None,
            },
            &facts,
            &RunHandles::default(),
            &unreachable_authority(),
            None,
        )
        .unwrap();
        // The replay window is a busy Run, not a missing one, so it uses a
        // registered code with the owner the contract asks for.
        for observed in [
            response,
            serve_events(
                &facts,
                &RunHandles::default(),
                0,
                EventProjection::Operational,
                8,
            ),
        ] {
            let ControlResponseV1::Failed {
                code,
                retryable,
                details,
                ..
            } = observed
            else {
                panic!("the replay window must refuse rather than answer");
            };
            assert_eq!(code, "RUN_BUSY");
            assert!(retryable, "a caller may come back once replay finishes");
            assert_eq!(details["run_id"], serde_json::json!(facts.run_id));
            assert_eq!(details["owner_kind"], "startup");
        }
    }

    #[test]
    fn observer_and_mutation_control_requests_are_classified_apart() {
        let expected = identity();
        for observer in [
            ControlRequestV1::Hello {
                expected: expected.clone(),
            },
            ControlRequestV1::Status {
                expected: expected.clone(),
            },
            ControlRequestV1::RunStatus {
                caller: None,
                expected: expected.clone(),
            },
            ControlRequestV1::Events {
                caller: None,
                expected: expected.clone(),
                after: 0,
                projection: EventProjection::Minimal,
                limit: 1,
            },
        ] {
            assert!(observer.observes_only());
            assert!(!observer.requires_controller());
            assert_eq!(observer.expected(), &expected);
        }
        for mutation in [
            ControlRequestV1::Wait {
                caller: None,
                expected: expected.clone(),
                turn_id: "turn-1".to_owned(),
                timeout_ms: None,
            },
            ControlRequestV1::Interrupt {
                caller: None,
                expected: expected.clone(),
            },
            ControlRequestV1::Close {
                caller: None,
                expected: expected.clone(),
                interrupt: false,
            },
        ] {
            assert!(!mutation.observes_only());
        }
    }

    #[test]
    fn only_state_changing_requests_demand_a_controller_credential() {
        let expected = identity();
        let turn = TurnControlRequest {
            message: "probe".to_owned(),
            idempotency_key: "probe".to_owned(),
            effort: None,
            images: Vec::new(),
        };
        // `wait` rejoins an already-authorized Turn and stays open with the
        // other observers; `shutdown` keeps frozen control-v1 identity-only
        // authorization.
        for open in [
            ControlRequestV1::Hello {
                expected: expected.clone(),
            },
            ControlRequestV1::Status {
                expected: expected.clone(),
            },
            ControlRequestV1::Shutdown {
                expected: expected.clone(),
            },
            ControlRequestV1::Wait {
                caller: None,
                expected: expected.clone(),
                turn_id: "turn-1".to_owned(),
                timeout_ms: None,
            },
            ControlRequestV1::Events {
                caller: None,
                expected: expected.clone(),
                after: 0,
                projection: EventProjection::Minimal,
                limit: 1,
            },
        ] {
            assert!(
                !open.requires_controller(),
                "{open:?} must stay open to same-uid observers"
            );
        }
        for mutation in [
            ControlRequestV1::Send {
                caller: None,
                expected: expected.clone(),
                request: turn.clone(),
                timeout_ms: None,
            },
            ControlRequestV1::Submit {
                caller: None,
                expected: expected.clone(),
                request: turn,
            },
            ControlRequestV1::Respond {
                caller: None,
                expected: expected.clone(),
                request_id: 1,
                idempotency_key: "response-1".to_owned(),
                response: Value::Null,
            },
            ControlRequestV1::Interrupt {
                caller: None,
                expected: expected.clone(),
            },
            ControlRequestV1::Close {
                caller: None,
                expected: expected.clone(),
                interrupt: false,
            },
        ] {
            assert!(
                mutation.requires_controller(),
                "{mutation:?} must be revalidated before its effect"
            );
            assert!(mutation.operation_name().starts_with("run."));
        }
    }

    #[test]
    fn a_mutation_without_a_descriptor_is_refused_before_the_run_is_reached() {
        // The authority names a Run root that does not exist, so any refusal
        // that consults durable state would surface a different code. Only a
        // refusal decided from the missing descriptor alone can be
        // CONTROLLER_MISMATCH here.
        let authority = unreachable_authority();
        assert_eq!(
            revalidate_controller(&authority, "run.send", None),
            Some(ControlResponseV1::Failed {
                code: "CONTROLLER_MISMATCH".to_owned(),
                message: "controller credential does not authorize run.send".to_owned(),
                retryable: false,
                details: serde_json::json!({
                    "run_id": authority.run_id(),
                    "operation": "run.send",
                }),
            })
        );
    }

    /// SPEC-006 has a caller-supplied timeout "return the current nonterminal
    /// state without interrupting the worker", which only means anything if
    /// the worker actually waited that long.
    ///
    /// Cutting an hour down to one mutation exchange's fifteen minutes would
    /// report a Turn as still running at a moment the caller never asked
    /// about, and would send a caller that named no timeout away instead of
    /// waiting for the Turn it asked for.  The ceiling is the one bound a
    /// held worker thread does need, and it is far above either.
    #[test]
    fn a_callers_wait_is_honoured_as_written_up_to_the_wait_ceiling() {
        assert_eq!(MAX_TURN_WAIT_TIMEOUT, Duration::from_secs(24 * 60 * 60));
        assert!(
            MAX_TURN_WAIT_TIMEOUT > CONTROL_CALL_TIMEOUT,
            "waiting for a turn has to be allowed to outlast one mutation call"
        );
        assert_eq!(caller_budget(None), None);
        for milliseconds in [1_u64, 200, 900_000, 900_001, 3_600_000] {
            assert_eq!(
                caller_budget(Some(milliseconds)),
                Some(Duration::from_millis(milliseconds)),
                "a caller timeout of {milliseconds}ms was quietly shortened"
            );
        }
        assert_eq!(caller_budget(Some(u64::MAX)), Some(MAX_TURN_WAIT_TIMEOUT));
    }

    #[test]
    fn socket_name_is_fixed_and_independent_of_tmpdir() {
        let identity = identity();
        let first =
            worker_socket_path(identity.uid, &identity.workspace_id, identity.run_id).unwrap();
        let second =
            worker_socket_path(identity.uid, &identity.workspace_id, identity.run_id).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.parent().unwrap(), Path::new("/tmp/dolgorae-501/s"));
        assert_eq!(first.file_stem().unwrap().len(), 32);
    }

    #[test]
    fn ordinary_rejects_skew_while_control_accepts_it() {
        let base = WorkerHello {
            schema_version: 1,
            identity: identity(),
            control_socket_epoch: 2,
            dolgorae_version: "0.1.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "33".repeat(32),
        };
        let mut skewed = base.clone();
        skewed.dolgorae_version = "0.2.0".to_owned();
        skewed.mutation_protocol_version = 2;
        skewed.binary_sha256 = "44".repeat(32);
        assert_eq!(
            skewed.validate_ordinary(&base),
            Err(WorkerProtocolError::ProtocolMismatch {
                expected_protocol: 1,
                actual_protocol: 2,
            })
        );
        assert_eq!(skewed.validate_control_v1(&base), Ok(()));
        // The same skew reaches an ordinary CLI request through the runtime
        // record, which is the comparison the worker's own identity check
        // cannot make: the CLI addresses it with the worker's own identity.
        let record = WorkerRuntimeRecord {
            schema_version: 1,
            identity: base.identity.clone(),
            socket_path: worker_socket_path(
                base.identity.uid,
                &base.identity.workspace_id,
                base.identity.run_id,
            )
            .unwrap(),
            socket_identity: SocketIdentity {
                device: 1,
                inode: 2,
            },
            control_socket_epoch: base.control_socket_epoch,
            app_server_epoch: None,
            dedicated_server_identity: None,
            dolgorae_version: base.dolgorae_version.clone(),
            mutation_protocol_version: base.mutation_protocol_version,
            binary_sha256: base.binary_sha256.clone(),
        };
        let current = ExecutingBuild {
            version: "0.2.0".to_owned(),
            mutation_protocol_version: 2,
            binary_sha256: "44".repeat(32),
        };
        assert_eq!(
            record.validate_ordinary_peer(&current),
            Err(WorkerProtocolError::ProtocolMismatch {
                expected_protocol: 2,
                actual_protocol: 1,
            })
        );
        assert_eq!(
            record.validate_ordinary_peer(&ExecutingBuild {
                version: base.dolgorae_version.clone(),
                mutation_protocol_version: base.mutation_protocol_version,
                binary_sha256: base.binary_sha256.clone(),
            }),
            Ok(())
        );
        for frozen in [
            ControlRequestV1::Hello {
                expected: base.identity.clone(),
            },
            ControlRequestV1::Status {
                expected: base.identity.clone(),
            },
            ControlRequestV1::Shutdown {
                expected: base.identity.clone(),
            },
        ] {
            assert!(
                frozen.frozen_control_v1(),
                "{} stays reachable across a build upgrade",
                frozen.operation_name()
            );
        }
        for ordinary in [
            ControlRequestV1::RunStatus {
                caller: None,
                expected: base.identity.clone(),
            },
            ControlRequestV1::Wait {
                caller: None,
                expected: base.identity.clone(),
                turn_id: "turn-1".to_owned(),
                timeout_ms: None,
            },
            ControlRequestV1::Interrupt {
                caller: None,
                expected: base.identity.clone(),
            },
            ControlRequestV1::Events {
                caller: None,
                expected: base.identity.clone(),
                after: 0,
                projection: EventProjection::Minimal,
                limit: 1,
            },
        ] {
            assert!(
                !ordinary.frozen_control_v1(),
                "{} must not cross a build upgrade",
                ordinary.operation_name()
            );
        }
    }

    #[test]
    fn control_never_accepts_cross_run_identity() {
        let base = WorkerHello {
            schema_version: 1,
            identity: identity(),
            control_socket_epoch: 2,
            dolgorae_version: "0.1.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "33".repeat(32),
        };
        let mut foreign = base.clone();
        foreign.identity.run_id = Uuid::now_v7();
        assert_eq!(
            foreign.validate_control_v1(&base),
            Err(WorkerProtocolError::ProtocolMismatch {
                expected_protocol: 1,
                actual_protocol: 1,
            })
        );
    }

    #[test]
    fn owner_slots_are_fixed_checked_and_zero_padded() {
        let record = StartupOwnerRecord {
            schema_version: 1,
            slot: 1,
            identity: identity(),
            executable_path_sha256: "55".repeat(32),
        };
        let encoded = record.encode_slot().unwrap();
        assert_eq!(encoded.len(), STARTUP_SLOT_BYTES);
        assert_eq!(
            StartupOwnerRecord::decode_slot(&encoded).unwrap(),
            Some(record)
        );
        let mut corrupt = encoded;
        corrupt[10] ^= 1;
        assert_eq!(
            StartupOwnerRecord::decode_slot(&corrupt),
            Err(WorkerProtocolError::InvalidOwnerRecord)
        );
        assert_eq!(
            StartupOwnerRecord::decode_slot(&[0; STARTUP_SLOT_BYTES]).unwrap(),
            None
        );
    }

    #[test]
    fn bounded_frames_recover_after_an_oversized_caller() {
        let mut wire = vec![b'x'; MAX_CLI_WORKER_FRAME_BYTES + 1];
        wire.extend_from_slice(b"\n{\"operation\":\"status\",\"expected\":");
        wire.extend_from_slice(&serde_json::to_vec(&identity()).unwrap());
        wire.extend_from_slice(b"}\n");
        let mut reader = BufReader::new(Cursor::new(wire));
        assert!(matches!(
            read_frame::<_, ControlRequestV1>(&mut reader),
            Err(WorkerProtocolError::FrameTooLarge { observed_bytes })
                if observed_bytes > MAX_CLI_WORKER_FRAME_BYTES as u64
        ));
        assert!(matches!(
            read_frame::<_, ControlRequestV1>(&mut reader).unwrap(),
            ControlRequestV1::Status { .. }
        ));
    }

    #[test]
    fn malformed_and_unknown_control_frames_are_rejected() {
        for input in [
            b"{}\n".as_slice(),
            b"{\"operation\":\"replay\"}\n",
            b"null\n",
            b"{\"operation\":\"hello\",\"expected\":{},\"extra\":1}\n",
        ] {
            let mut reader = BufReader::new(Cursor::new(input));
            assert_eq!(
                read_frame::<_, ControlRequestV1>(&mut reader),
                Err(WorkerProtocolError::MalformedFrame)
            );
        }
    }

    #[test]
    fn socket_bind_refuses_foreign_occupants_without_unlinking() {
        let mut identity = identity();
        identity.uid = fs::metadata(".").unwrap().uid();
        let first = bind_worker_socket(&identity, None).unwrap();
        let path = first.path().to_owned();
        drop(first);
        fs::write(&path, b"foreign").unwrap();
        assert!(matches!(
            bind_worker_socket(&identity, None),
            Err(WorkerProtocolError::SocketIdentityMismatch)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"foreign");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn verified_stale_socket_is_replaced_and_lease_cleans_only_its_inode() {
        let mut identity = identity();
        identity.uid = fs::metadata(".").unwrap().uid();
        let initial = bind_worker_socket(&identity, None).unwrap();
        let path = initial.path().to_owned();
        drop(initial);
        let stale_listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let stale_identity = SocketIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        drop(stale_listener);
        let record = WorkerRuntimeRecord {
            schema_version: 1,
            identity: identity.clone(),
            socket_path: path.clone(),
            socket_identity: stale_identity.clone(),
            control_socket_epoch: 1,
            app_server_epoch: None,
            dedicated_server_identity: None,
            dolgorae_version: "0.1.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "33".repeat(32),
        };
        let authority =
            VerifiedStaleSocket::from_absent_generation(&record, stale_identity).unwrap();
        let replacement = bind_worker_socket(&identity, Some(authority)).unwrap();
        assert_ne!(replacement.identity(), &record.socket_identity);
        drop(replacement);
        assert!(!path.exists());
    }

    #[test]
    fn startup_file_is_permanent_fixed_size_and_revalidates_path_identity() {
        let root = std::env::temp_dir().join(format!("dolgorae-worker-test-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("run.lock");
        let uid = fs::metadata(&root).unwrap().uid();
        let lock = StartupLockFile::open(&path, uid).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            STARTUP_LOCK_BYTES as u64
        );
        let owner = StartupOwnerRecord {
            schema_version: 1,
            slot: 1,
            identity: identity(),
            executable_path_sha256: "55".repeat(32),
        };
        lock.write_owner(&owner).unwrap();
        assert_eq!(lock.read_owner(1).unwrap(), Some(owner));
        fs::rename(&path, root.join("replaced")).unwrap();
        fs::write(&path, vec![0_u8; STARTUP_LOCK_BYTES]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            lock.clear_owner(1),
            Err(WorkerProtocolError::SocketIdentityMismatch)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn startup_byte_one_has_one_cross_process_owner_and_byte_zero_remains_independent() {
        if let Ok(root) = std::env::var("DOLGORAE_LOCK_TEST_CHILD") {
            let root = PathBuf::from(root);
            let path = root.join("run.lock");
            let uid = fs::metadata(&root).unwrap().uid();
            let lock = StartupLockFile::open(&path, uid).unwrap();
            let mut owner_identity = identity();
            owner_identity.uid = uid;
            let owner = StartupOwnerRecord {
                schema_version: 1,
                slot: 1,
                identity: owner_identity,
                executable_path_sha256: "55".repeat(32),
            };
            let guard = lock.claim(&owner, Duration::from_secs(1)).unwrap();
            fs::write(root.join("ready"), b"ready").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !root.join("release").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            guard.release().unwrap();
            return;
        }

        let root = std::env::temp_dir().join(format!("dolgorae-lock-race-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("run.lock");
        let uid = fs::metadata(&root).unwrap().uid();
        let lock = StartupLockFile::open(&path, uid).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("worker::tests::startup_byte_one_has_one_cross_process_owner_and_byte_zero_remains_independent")
            .arg("--nocapture")
            .env("DOLGORAE_LOCK_TEST_CHILD", &root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.join("ready").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(root.join("ready").exists(), "child did not acquire byte 1");

        let mut owner_identity = identity();
        owner_identity.uid = uid;
        let byte_zero = StartupOwnerRecord {
            schema_version: 1,
            slot: 0,
            identity: owner_identity.clone(),
            executable_path_sha256: "55".repeat(32),
        };
        let zero_guard = lock.claim(&byte_zero, Duration::from_millis(100)).unwrap();
        let byte_one = StartupOwnerRecord {
            schema_version: 1,
            slot: 1,
            identity: owner_identity,
            executable_path_sha256: "55".repeat(32),
        };
        assert!(matches!(
            lock.claim(&byte_one, Duration::from_millis(100)),
            Err(WorkerProtocolError::StartupBusy)
        ));
        assert!(lock.read_owner(1).unwrap().is_some());
        zero_guard.release().unwrap();
        fs::write(root.join("release"), b"release").unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(lock.read_owner(1).unwrap(), None);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn foreign_thread_observations_land_in_this_runs_profile_diagnostic_journal() {
        let dolgorae_home =
            std::env::temp_dir().join(format!("dolgorae-foreign-sink-{}", Uuid::now_v7()));
        let state_root = dolgorae_home.join("workspaces").join("11".repeat(32));
        fs::create_dir_all(&state_root).unwrap();
        let server_key = "a".repeat(64);

        let profile_root = profile_diagnostics_root(&state_root, &server_key).unwrap();
        assert_eq!(
            profile_root,
            dolgorae_home.join("profiles").join(&server_key),
            "a Run's diagnostics belong to the Runtime Profile it is pinned to"
        );
        assert!(
            profile_diagnostics_root(&state_root, "not-a-server-key").is_err(),
            "an unusable server key is a bootstrap fault, not a path to guess at"
        );

        let sink = ProfileForeignDiagnostics {
            profile_root: profile_root.clone(),
        };
        for request_id in 0..3_u64 {
            sink.record(
                &IgnoredForeignRequest {
                    request_id,
                    method: "item/tool/requestUserInput".to_owned(),
                    thread_id: Some("thread-other".to_owned()),
                    turn_id: Some("turn-other".to_owned()),
                },
                ForeignLane {
                    run_generation: 2,
                    server_key: &server_key,
                    server_epoch: 5,
                },
            )
            .unwrap();
        }

        // Append-only and replayable in order, one line per observation.
        let journal = fs::read_to_string(profile_root.join("diagnostics.jsonl")).unwrap();
        let records: Vec<Value> = journal
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 3);
        let ids: Vec<u64> = records
            .iter()
            .map(|record| record["details"]["request_id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![0, 1, 2]);
        for record in &records {
            assert_eq!(record["kind"], "foreign_thread_request_ignored");
            assert_eq!(
                record["projection"], "operational",
                "foreign-thread routing metadata is gated on the operator capability"
            );
            assert_eq!(record["details"]["server_epoch"], 5);
            assert_eq!(
                record["details"].as_object().unwrap().len(),
                7,
                "the recorded details are a closed set of routing members"
            );
        }
        assert_eq!(
            fs::metadata(profile_root.join("diagnostics.jsonl"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(&dolgorae_home).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_reset_holding_the_startup_range_and_a_start_election_exclude_each_other() {
        // POSIX record locks are per process, so both directions have to be
        // proven across a real process boundary; the child is this same test
        // binary re-entered on the one case.
        if let Ok(root) = std::env::var("DOLGORAE_RESET_RANGE_CHILD") {
            let root = PathBuf::from(root);
            let path = root.join("run.lock");
            let uid = fs::metadata(&root).unwrap().uid();
            let lock = StartupLockFile::open(&path, uid).unwrap();

            // Direction one: the reset holds the range first.
            lock.hold_startup_range(Duration::from_secs(1)).unwrap();
            fs::write(root.join("reset-held"), b"held").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !root.join("start-was-blocked").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            lock.release_startup_range().unwrap();
            fs::write(root.join("reset-released"), b"released").unwrap();

            // Direction two: the start election holds it first.
            while !root.join("start-elected").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let refused = lock.hold_startup_range(Duration::ZERO);
            assert!(
                matches!(refused, Err(WorkerProtocolError::StartupBusy)),
                "a reset must not take the range a start election holds"
            );
            fs::write(root.join("reset-was-blocked"), b"blocked").unwrap();
            return;
        }

        let root = std::env::temp_dir().join(format!("dolgorae-reset-range-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("run.lock");
        let uid = fs::metadata(&root).unwrap().uid();
        let lock = StartupLockFile::open(&path, uid).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("worker::tests::a_reset_holding_the_startup_range_and_a_start_election_exclude_each_other")
            .arg("--nocapture")
            .env("DOLGORAE_RESET_RANGE_CHILD", &root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let await_file = |name: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !root.join(name).exists() {
                assert!(Instant::now() < deadline, "child never wrote {name}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        await_file("reset-held");
        let mut owner_identity = identity();
        owner_identity.uid = uid;
        let election = StartupOwnerRecord {
            schema_version: 1,
            slot: 0,
            identity: owner_identity,
            executable_path_sha256: "55".repeat(32),
        };
        assert!(
            matches!(
                lock.claim(&election, Duration::from_millis(100)),
                Err(WorkerProtocolError::StartupBusy)
            ),
            "a worker start election must not win the range a reset holds"
        );
        // The reset holds the range with slot 0's owner record left clear: an
        // all-zero slot never establishes identity, so a contender sees a
        // locked, unverifiable range and reports RUN_BUSY rather than
        // adopting a stale owner.
        assert_eq!(lock.read_owner(0).unwrap(), None);
        fs::write(root.join("start-was-blocked"), b"blocked").unwrap();

        await_file("reset-released");
        let guard = lock.claim(&election, Duration::from_secs(5)).unwrap();
        fs::write(root.join("start-elected"), b"elected").unwrap();
        await_file("reset-was-blocked");
        guard.release().unwrap();

        assert!(child.wait().unwrap().success());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn control_server_isolates_slow_callers_and_interrupts_before_shutdown() {
        let mut worker_identity = identity();
        worker_identity.uid = fs::metadata(".").unwrap().uid();
        let lease = bind_worker_socket(&worker_identity, None).unwrap();
        let path = lease.path().to_owned();
        let hello = WorkerHello {
            schema_version: 1,
            identity: worker_identity.clone(),
            control_socket_epoch: 1,
            dolgorae_version: "0.1.0".to_owned(),
            mutation_protocol_version: 1,
            binary_sha256: "33".repeat(32),
        };
        let server = WorkerControlServer::new(
            hello,
            RunFacts {
                run_id: worker_identity.run_id,
                profile: "default".to_owned(),
            },
            WorkerControlState {
                lifecycle: "running".to_owned(),
                active_turn: Some("turn-1".to_owned()),
            },
            unreachable_authority(),
        );
        let server_thread = thread::spawn(move || server.serve(&lease));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut slow = loop {
            match UnixStream::connect(&path) {
                Ok(stream) => break stream,
                Err(_) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("control socket did not become ready: {error}"),
            }
        };
        slow.write_all(b"{\"operation\":").unwrap();

        let status = control_call(
            &path,
            &ControlRequestV1::Status {
                expected: worker_identity.clone(),
            },
        );
        assert!(matches!(
            status,
            ControlResponseV1::Status {
                lifecycle,
                active_turn: Some(turn),
                ..
            } if lifecycle == "running" && turn == "turn-1"
        ));
        let shutdown = control_call(
            &path,
            &ControlRequestV1::Shutdown {
                expected: worker_identity,
            },
        );
        assert!(matches!(
            shutdown,
            ControlResponseV1::Shutdown {
                terminal_confirmed: true,
                ..
            }
        ));
        drop(slow);
        assert_eq!(server_thread.join().unwrap(), Ok(()));
        assert!(!path.exists());
    }

    /// A Run root that does not exist, for cases where no request under test
    /// is allowed to reach Controller authorization at all.
    fn unreachable_authority() -> RunControllerAuthority {
        RunControllerAuthority::new(
            PathBuf::from("/nonexistent/dolgorae-unreachable-authority"),
            identity().run_id,
        )
    }

    fn control_call(path: &Path, request: &ControlRequestV1) -> ControlResponseV1 {
        let mut stream = UnixStream::connect(path).unwrap();
        write_frame(&mut stream, request).unwrap();
        let mut reader = BufReader::new(stream);
        read_frame(&mut reader).unwrap()
    }
}
