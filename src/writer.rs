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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterRecord {
    pub schema_version: u32,
    pub workspace_id: String,
    pub authority_revision: u64,
    pub state: WriterAuthorityState,
    pub writer_generation: u64,
    pub transaction_id: Option<Uuid>,
    pub holder: Option<WriterHolder>,
    pub handoff: Option<WriterHandoff>,
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
            workspace_id: workspace_id.to_owned(),
            authority_revision: 0,
            state: WriterAuthorityState::None,
            writer_generation: 0,
            transaction_id: None,
            holder: None,
            handoff: None,
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
        if self.schema_version != 1
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
        self.state = WriterAuthorityState::Active;
        self.transaction_id = None;
        Ok(())
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

    pub fn block_unknown(&mut self, transaction_id: Uuid) {
        if self.transaction_id == Some(transaction_id) {
            self.state = WriterAuthorityState::BlockedUnknown;
            self.handoff = None;
            self.recovery_action = Some("reverify_writer_policy".to_owned());
        }
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
        Ok(())
    }

    pub fn cancel_handoff(&mut self, handoff_id: Uuid) -> Result<(), MachineError> {
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
        self.handoff = None;
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

    pub fn load(&self) -> Result<WriterRecord, MachineError> {
        verify_secure_file(&self.record_path(), self.uid)?;
        let bytes =
            fs::read(self.record_path()).map_err(|error| path_error(&self.record_path(), error))?;
        if bytes.len() > 64 * 1024 {
            return Err(invariant("writer authority record exceeds 64 KiB"));
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
        let result = operation(&mut record)?;
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

    pub fn status_value(&self) -> Result<Value, MachineError> {
        let record = self.load()?;
        let holder = record.holder.as_ref();
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
            "thread_id": holder.and_then(|value| value.thread_id.clone()),
            "active_turn_id": Value::Null,
            "lifecycle_state": holder.map(|value| value.lifecycle.as_str()),
            "pending_interaction_count": 0,
            "last_event_cursor": "0",
            "recovery_state": if record.state == WriterAuthorityState::None { "ready" } else { "unverifiable" },
            "execution_lane": "dedicated",
            "dedicated_lane": Value::Null,
            "workload_background_state": {
                "state": "unverified",
                "mechanism": "dedicated_lane_process_census",
                "census_revision": record.authority_revision,
                "observed_process_count": 0,
                "quiescent_since": Value::Null,
                "consecutive_empty_samples": 0
            },
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
            return Err(MachineError::runtime_path_invalid(
                &path,
                "held descriptor and lock pathname differ",
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
            return Err(MachineError::runtime_path_invalid(
                &path,
                "permanent lock pathname identity changed",
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
    use std::os::unix::fs::PermissionsExt;

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
        assert_eq!(error.code, "RUNTIME_PATH_INVALID");
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
        assert_eq!(error.code, "RUNTIME_PATH_INVALID");
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
