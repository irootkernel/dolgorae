//! Per-Run worker discovery and the private CLI-to-worker protocol.
//!
//! This module deliberately keeps process supervision behind checked data
//! contracts.  The CLI and hidden worker share these routines, so neither side
//! can silently relax identity, framing, or runtime-record validation.

#[cfg(target_os = "macos")]
use crate::conformance::ConformantLedger;
#[cfg(target_os = "macos")]
use crate::darwin::DarwinSystem;
use crate::event::EventProjection;
use crate::fault::FaultInjector;
use crate::ledger::{Ledger, LedgerClock};
use crate::machine::MachineError;
use data_encoding::{BASE32_NOPAD, HEXLOWER};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Write};
use std::os::unix::fs::{
    DirBuilderExt as _, FileExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _,
    PermissionsExt as _,
};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Child, Command};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

pub const WORKER_PROTOCOL_VERSION: u32 = 1;
pub const CONTROL_PROTOCOL_VERSION: u32 = 1;
pub const MAX_CLI_WORKER_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const STARTUP_SLOT_BYTES: usize = 4096;
pub const STARTUP_LOCK_BYTES: usize = STARTUP_SLOT_BYTES * 2;
pub const STARTUP_HANDOFF_FD: i32 = 3;
pub const MAX_STARTUP_HANDOFF_BYTES: usize = 64 * 1024;

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
    pub uid: u32,
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
    pub ledger_root: PathBuf,
    pub runtime_record_path: PathBuf,
    pub startup_lock_path: PathBuf,
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
            || decode_sha256(&self.executable_sha256).is_none()
        {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        Ok(())
    }
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
    pub dolgorae_version: String,
    pub mutation_protocol_version: u32,
    pub binary_sha256: String,
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
        reason = "the TASK-004 process-identity prober mints this sealed capability"
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
            return Err(WorkerProtocolError::ProtocolMismatch);
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
            return Err(WorkerProtocolError::ProtocolMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlRequestV1 {
    Hello { expected: WorkerIdentity },
    Status { expected: WorkerIdentity },
    Shutdown { expected: WorkerIdentity },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlResponseV1 {
    Hello {
        hello: WorkerHello,
    },
    Status {
        identity: WorkerIdentity,
        lifecycle: String,
        active_turn: Option<String>,
    },
    Shutdown {
        identity: WorkerIdentity,
        terminal_confirmed: bool,
    },
    Rejected {
        code: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerControlState {
    pub lifecycle: String,
    pub active_turn: Option<String>,
    pub terminal_on_shutdown: bool,
}

#[derive(Clone)]
pub struct WorkerControlServer {
    hello: WorkerHello,
    state: Arc<Mutex<WorkerControlState>>,
    stopping: Arc<AtomicBool>,
}

impl WorkerControlServer {
    #[must_use]
    pub fn new(hello: WorkerHello, state: WorkerControlState) -> Self {
        Self {
            hello,
            state: Arc::new(Mutex::new(state)),
            stopping: Arc::new(AtomicBool::new(false)),
        }
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

    pub fn terminate(&self) -> Result<(), WorkerProtocolError> {
        let mut state = self.state.lock().map_err(|_| WorkerProtocolError::Io)?;
        if state.active_turn.is_some() && state.terminal_on_shutdown {
            state.active_turn = None;
            state.lifecycle = "idle".to_owned();
        }
        drop(state);
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
                    let hello = self.hello.clone();
                    let state = Arc::clone(&self.state);
                    let stopping = Arc::clone(&stopping);
                    callers.push(thread::spawn(move || {
                        let _ = serve_control_caller(stream, &hello, &state, &stopping);
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return Err(WorkerProtocolError::Io),
            }
        }
        for caller in callers {
            caller.join().map_err(|_| WorkerProtocolError::Io)?;
        }
        Ok(())
    }
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
    FrameTooLarge,
    MalformedFrame,
    InvalidIdentity,
    InvalidRuntimeRecord,
    InvalidOwnerRecord,
    ProtocolMismatch,
    SocketIdentityMismatch,
    StartupBusy,
    LedgerReplay,
    WorkerStartFailed,
    Io,
}

impl WorkerProtocolError {
    #[must_use]
    pub fn machine_error(self) -> MachineError {
        let (code, message) = match self {
            Self::FrameTooLarge => (
                "PROTOCOL_FRAME_TOO_LARGE",
                "private worker frame exceeds the v1 byte limit",
            ),
            Self::ProtocolMismatch => (
                "DOLGORAE_PROTOCOL_MISMATCH",
                "private worker identity or protocol does not match",
            ),
            Self::SocketIdentityMismatch
            | Self::InvalidRuntimeRecord
            | Self::InvalidOwnerRecord => (
                "RUNTIME_PATH_COLLISION",
                "private worker runtime identity is unsafe or inconsistent",
            ),
            Self::StartupBusy => ("RUN_BUSY", "another process owns worker startup"),
            Self::LedgerReplay => ("AUDIT_INTEGRITY_FAILURE", "worker ledger replay failed"),
            Self::WorkerStartFailed => ("TRANSPORT_FAILURE", "hidden worker startup failed"),
            Self::MalformedFrame | Self::InvalidIdentity | Self::Io => {
                ("TRANSPORT_FAILURE", "private worker transport failed")
            }
        };
        MachineError::new(code, message, false, serde_json::json!({}))
    }
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
        return Err(WorkerProtocolError::FrameTooLarge);
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
        return Err(WorkerProtocolError::FrameTooLarge);
    }
    serde_json::from_slice(&bytes).map_err(|_| WorkerProtocolError::MalformedFrame)
}

#[cfg(target_os = "macos")]
pub fn spawn_hidden_worker(
    executable: &Path,
    bootstrap_path: &Path,
) -> Result<StartedWorker, WorkerProtocolError> {
    if !executable.is_absolute() || !bootstrap_path.is_absolute() {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    let uid = DarwinSystem.current_uid();
    let bootstrap = read_worker_bootstrap(bootstrap_path, uid)?;
    let process = DarwinSystem
        .current_process()
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    let election_identity = WorkerIdentity {
        workspace_id: bootstrap.workspace_id.clone(),
        run_id: bootstrap.run_id,
        run_generation: bootstrap.run_generation,
        boot_uuid: bootstrap.boot_uuid,
        pid: process.pid,
        process_group_id: process.process_group_id,
        uid: process.uid,
        executable_sha256: bootstrap.executable_sha256.clone(),
    };
    let startup_lock = StartupLockFile::open(&bootstrap.startup_lock_path, uid)?;
    let election = startup_lock.claim(
        &StartupOwnerRecord {
            schema_version: 1,
            slot: 0,
            identity: election_identity,
            executable_path_sha256: bootstrap.executable_path_sha256.clone(),
        },
        Duration::from_secs(10),
    )?;
    let startup_result = (|| {
        let mut command = Command::new(executable);
        command
            .arg("__worker")
            .arg("--bootstrap")
            .arg(bootstrap_path);
        let (child, startup) = DarwinSystem
            .spawn_detached_with_fd3(&mut command)
            .map_err(|_| WorkerProtocolError::Io)?;
        startup
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|_| WorkerProtocolError::Io)?;
        let mut reader = std::io::BufReader::new(startup);
        let bound = match read_startup_handoff(&mut reader)? {
            StartupHandoff::Bound { record } => record,
            StartupHandoff::Failed { .. } => return Err(WorkerProtocolError::WorkerStartFailed),
            StartupHandoff::Ready { .. } => return Err(WorkerProtocolError::MalformedFrame),
        };
        Ok((child, reader, bound))
    })();
    let release = election.release();
    let (mut child, mut reader, bound) = startup_result?;
    release?;
    if bound.identity.workspace_id != bootstrap.workspace_id
        || bound.identity.run_id != bootstrap.run_id
        || bound.identity.run_generation != bootstrap.run_generation
        || bound.identity.boot_uuid != bootstrap.boot_uuid
        || bound.identity.uid != uid
        || bound.identity.executable_sha256 != bootstrap.executable_sha256
    {
        return Err(WorkerProtocolError::ProtocolMismatch);
    }
    let ready = match read_startup_handoff(&mut reader)? {
        StartupHandoff::Ready { record } => record,
        StartupHandoff::Failed { .. } => return Err(WorkerProtocolError::WorkerStartFailed),
        StartupHandoff::Bound { .. } => return Err(WorkerProtocolError::MalformedFrame),
    };
    if bound != ready
        || ready.identity.pid != child.id()
        || ready.identity.process_group_id != child.id()
    {
        return Err(WorkerProtocolError::ProtocolMismatch);
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
    let identity = WorkerIdentity {
        workspace_id: bootstrap.workspace_id.clone(),
        run_id: bootstrap.run_id,
        run_generation: bootstrap.run_generation,
        boot_uuid: bootstrap.boot_uuid,
        pid: process.pid,
        process_group_id: process.process_group_id,
        uid: process.uid,
        executable_sha256: bootstrap.executable_sha256.clone(),
    };
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
    let record = WorkerRuntimeRecord {
        schema_version: 1,
        identity: identity.clone(),
        socket_path: lease.path().to_owned(),
        socket_identity: lease.identity().clone(),
        control_socket_epoch: bootstrap.control_socket_epoch,
        dolgorae_version: bootstrap.dolgorae_version,
        mutation_protocol_version: bootstrap.mutation_protocol_version,
        binary_sha256: bootstrap.executable_sha256,
    };
    write_runtime_record(&bootstrap.runtime_record_path, &record)?;
    write_startup_handoff(&StartupHandoff::Bound {
        record: record.clone(),
    })?;
    let control = WorkerControlServer::new(
        WorkerHello {
            schema_version: 1,
            identity,
            control_socket_epoch: record.control_socket_epoch,
            dolgorae_version: record.dolgorae_version.clone(),
            mutation_protocol_version: record.mutation_protocol_version,
            binary_sha256: record.binary_sha256.clone(),
        },
        WorkerControlState {
            lifecycle: "replaying".to_owned(),
            active_turn: None,
            terminal_on_shutdown: true,
        },
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
            let ledger = ConformantLedger::open(&bootstrap.ledger_root, bootstrap.run_id)
                .map_err(|_| WorkerProtocolError::LedgerReplay)?;
            let projection = ledger
                .inner()
                .projection()
                .map_err(|_| WorkerProtocolError::LedgerReplay)?;
            if control.is_stopping() {
                return Err(WorkerProtocolError::WorkerStartFailed);
            }
            let lifecycle = serde_json::to_value(projection.lifecycle)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or(WorkerProtocolError::LedgerReplay)?;
            control.replace_state(WorkerControlState {
                lifecycle,
                active_turn: projection.active_turn_id,
                terminal_on_shutdown: true,
            })?;
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
        drop(ledger);
        served
    });
    let release = owner_guard.release();
    result?;
    release
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

pub fn write_frame<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), WorkerProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| WorkerProtocolError::MalformedFrame)?;
    if bytes.len() > MAX_CLI_WORKER_FRAME_BYTES {
        return Err(WorkerProtocolError::FrameTooLarge);
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

pub fn read_frame<R: BufRead, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<T, WorkerProtocolError> {
    let mut frame = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().map_err(|_| WorkerProtocolError::Io)?;
        if available.is_empty() {
            return Err(WorkerProtocolError::MalformedFrame);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let payload = newline.map_or(available, |index| &available[..index]);
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
        return Err(WorkerProtocolError::FrameTooLarge);
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
    if socket_parent != expected_root.join("s") {
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

fn slot_offset(slot: u8) -> Result<u64, WorkerProtocolError> {
    match slot {
        0 => Ok(0),
        1 => Ok(STARTUP_SLOT_BYTES as u64),
        _ => Err(WorkerProtocolError::InvalidOwnerRecord),
    }
}

fn serve_control_caller(
    mut stream: UnixStream,
    hello: &WorkerHello,
    state: &Arc<Mutex<WorkerControlState>>,
    stopping: &AtomicBool,
) -> Result<(), WorkerProtocolError> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| WorkerProtocolError::Io)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|_| WorkerProtocolError::Io)?;
    let reader_stream = stream.try_clone().map_err(|_| WorkerProtocolError::Io)?;
    let mut reader = std::io::BufReader::new(reader_stream);
    let request: ControlRequestV1 = read_frame(&mut reader)?;
    let expected = match &request {
        ControlRequestV1::Hello { expected }
        | ControlRequestV1::Status { expected }
        | ControlRequestV1::Shutdown { expected } => expected,
    };
    if expected != &hello.identity {
        return write_frame(
            &mut stream,
            &ControlResponseV1::Rejected {
                code: "DOLGORAE_PROTOCOL_MISMATCH".to_owned(),
            },
        );
    }
    match request {
        ControlRequestV1::Hello { .. } => write_frame(
            &mut stream,
            &ControlResponseV1::Hello {
                hello: hello.clone(),
            },
        ),
        ControlRequestV1::Status { .. } => {
            let snapshot = state.lock().map_err(|_| WorkerProtocolError::Io)?.clone();
            write_frame(
                &mut stream,
                &ControlResponseV1::Status {
                    identity: hello.identity.clone(),
                    lifecycle: snapshot.lifecycle,
                    active_turn: snapshot.active_turn,
                },
            )
        }
        ControlRequestV1::Shutdown { .. } => {
            let terminal_confirmed = {
                let mut snapshot = state.lock().map_err(|_| WorkerProtocolError::Io)?;
                if snapshot.active_turn.is_some() && snapshot.terminal_on_shutdown {
                    snapshot.active_turn = None;
                    snapshot.lifecycle = "idle".to_owned();
                }
                snapshot.active_turn.is_none()
            };
            write_frame(
                &mut stream,
                &ControlResponseV1::Shutdown {
                    identity: hello.identity.clone(),
                    terminal_confirmed,
                },
            )?;
            stopping.store(true, Ordering::Release);
            Ok(())
        }
    }
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
            uid: 501,
            executable_sha256: "22".repeat(32),
        }
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
            Err(WorkerProtocolError::ProtocolMismatch)
        );
        assert_eq!(skewed.validate_control_v1(&base), Ok(()));
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
            Err(WorkerProtocolError::ProtocolMismatch)
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
        assert_eq!(
            read_frame::<_, ControlRequestV1>(&mut reader),
            Err(WorkerProtocolError::FrameTooLarge)
        );
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
            WorkerControlState {
                lifecycle: "running".to_owned(),
                active_turn: Some("turn-1".to_owned()),
                terminal_on_shutdown: true,
            },
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

    fn control_call(path: &Path, request: &ControlRequestV1) -> ControlResponseV1 {
        let mut stream = UnixStream::connect(path).unwrap();
        write_frame(&mut stream, request).unwrap();
        let mut reader = BufReader::new(stream);
        read_frame(&mut reader).unwrap()
    }
}
