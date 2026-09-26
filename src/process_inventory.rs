//! Boot-scoped ownership evidence for detached Dolgorae processes.
//!
//! This index lives outside the caller's disposable HOME. A missing owner is
//! necessary, but never sufficient, authority to signal a process.

use crate::darwin::{BsdProcessIdentity, DarwinSystem};
use crate::machine::MachineError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{
    DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_RECORD_BYTES: u64 = 32 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    id: Uuid,
    kind: String,
    owner_root: PathBuf,
    owner_device: u64,
    owner_inode: u64,
    boot_uuid: String,
    executable: PathBuf,
    executable_device: u64,
    executable_inode: u64,
    expected_argument: String,
    socket: Option<PathBuf>,
    profile: Option<String>,
    run_id: Option<Uuid>,
    process: Option<BsdProcessIdentity>,
    fingerprint: Option<String>,
    socket_device: Option<u64>,
    socket_inode: Option<u64>,
}

pub struct Registration {
    path: PathBuf,
    record: Record,
    activated: bool,
    spawned: bool,
    spawned_process: Option<BsdProcessIdentity>,
}

#[derive(Default)]
struct Selection {
    owner_roots: BTreeSet<PathBuf>,
    owner_root_under: BTreeSet<PathBuf>,
    kinds: BTreeSet<String>,
    profiles: BTreeSet<String>,
    run_ids: BTreeSet<Uuid>,
    pids: BTreeSet<u32>,
    all: bool,
    confirm: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Owned,
    Orphan,
    Absent,
    Unverifiable,
}

#[derive(Serialize)]
struct Candidate {
    id: Uuid,
    kind: String,
    owner_root: PathBuf,
    profile: Option<String>,
    run_id: Option<Uuid>,
    pid: Option<u32>,
    process_group_id: Option<u32>,
    socket: Option<PathBuf>,
    verdict: Verdict,
}

fn unsafe_state(reason: impl Into<String>) -> MachineError {
    MachineError::new(
        "ORPHAN_STATE_UNVERIFIABLE",
        "detached process ownership cannot be verified",
        false,
        json!({"reason": reason.into()}),
    )
}

fn changed_selection(reason: &'static str) -> MachineError {
    MachineError::new(
        "ORPHAN_SELECTION_CHANGED",
        "orphan selection changed after inspection",
        true,
        json!({"reason": reason}),
    )
}

fn root() -> Result<PathBuf, MachineError> {
    let uid = DarwinSystem.current_uid();
    let parent = PathBuf::from(format!("/tmp/dolgorae-{uid}"));
    let directory = parent.join("processes");
    for path in [&parent, &directory] {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(unsafe_state(error.to_string())),
        }
        let metadata =
            fs::symlink_metadata(path).map_err(|error| unsafe_state(error.to_string()))?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(unsafe_state("process inventory directory is not private"));
        }
    }
    Ok(directory)
}

fn write_record(path: &Path, record: &Record, replace: bool) -> Result<(), MachineError> {
    let bytes = serde_json::to_vec(record).map_err(|error| unsafe_state(error.to_string()))?;
    if bytes.len() > MAX_RECORD_BYTES as usize {
        return Err(unsafe_state("process inventory record exceeds its bound"));
    }
    let temporary = path.with_extension(format!("{}.tmp", Uuid::now_v7()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| unsafe_state(error.to_string()))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| unsafe_state(error.to_string()))?;
        if replace {
            fs::rename(&temporary, path).map_err(|error| unsafe_state(error.to_string()))?;
        } else {
            fs::hard_link(&temporary, path).map_err(|error| unsafe_state(error.to_string()))?;
            fs::remove_file(&temporary).map_err(|error| unsafe_state(error.to_string()))?;
        }
        File::open(path.parent().expect("record has parent"))
            .and_then(|directory| directory.sync_all())
            .map_err(|error| unsafe_state(error.to_string()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

impl Drop for Registration {
    fn drop(&mut self) {
        if !self.spawned && !self.activated && self.record.process.is_none() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Registration {
    pub fn prepare(
        kind: &str,
        owner_root: &Path,
        executable: &Path,
        expected_argument: String,
        socket: Option<&Path>,
        profile: Option<&str>,
        run_id: Option<Uuid>,
    ) -> Result<Self, MachineError> {
        if !matches!(
            kind,
            "profile_server" | "profile_log_drainer" | "dedicated_server" | "worker"
        ) || !owner_root.is_absolute()
            || !executable.is_absolute()
            || expected_argument.is_empty()
        {
            return Err(MachineError::invalid_argument(
                "process inventory",
                "invalid process registration",
            ));
        }
        let owner_root =
            fs::canonicalize(owner_root).map_err(|error| unsafe_state(error.to_string()))?;
        let owner =
            fs::symlink_metadata(&owner_root).map_err(|error| unsafe_state(error.to_string()))?;
        if !owner.file_type().is_dir() || owner.uid() != DarwinSystem.current_uid() {
            return Err(unsafe_state(
                "process owner root is not a same-user directory",
            ));
        }
        let image = fs::metadata(executable).map_err(|error| unsafe_state(error.to_string()))?;
        if !image.file_type().is_file() {
            return Err(unsafe_state("registered executable is not a regular file"));
        }
        let directory = root()?;
        let id = Uuid::now_v7();
        let path = directory.join(format!("{id}.json"));
        let record = Record {
            schema_version: 1,
            id,
            kind: kind.to_owned(),
            owner_root,
            owner_device: owner.dev(),
            owner_inode: owner.ino(),
            boot_uuid: DarwinSystem
                .boot_session_uuid()
                .map_err(|error| unsafe_state(error.to_string()))?,
            executable: executable.to_path_buf(),
            executable_device: image.dev(),
            executable_inode: image.ino(),
            expected_argument,
            socket: socket.map(Path::to_path_buf),
            profile: profile.map(str::to_owned),
            run_id,
            process: None,
            fingerprint: None,
            socket_device: None,
            socket_inode: None,
        };
        write_record(&path, &record, false)?;
        Ok(Self {
            path,
            record,
            activated: false,
            spawned: false,
            spawned_process: None,
        })
    }

    pub fn mark_spawned(&mut self, pid: u32) {
        self.spawned = true;
        self.spawned_process = DarwinSystem
            .bsd_process_identity(pid)
            .ok()
            .filter(|process| {
                process.pid == pid
                    && process.process_group_id == pid
                    && process.session_id == pid
                    && process.uid == DarwinSystem.current_uid()
            });
    }

    pub fn abort_spawn(&mut self) -> Result<(), MachineError> {
        let process = self
            .record
            .process
            .or(self.spawned_process)
            .ok_or_else(|| unsafe_state("spawned process identity is unavailable"))?;
        if !verify_group(&process)?.is_empty() {
            DarwinSystem
                .signal_process_group(process.process_group_id, libc::SIGTERM)
                .map_err(|error| unsafe_state(error.to_string()))?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && !verify_group(&process)?.is_empty() {
                std::thread::sleep(Duration::from_millis(100));
            }
            if !verify_group(&process)?.is_empty() {
                DarwinSystem
                    .signal_process_group(process.process_group_id, libc::SIGKILL)
                    .map_err(|error| unsafe_state(error.to_string()))?;
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline && !verify_group(&process)?.is_empty() {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        for sample in 0..5 {
            if !verify_group(&process)?.is_empty() {
                return Err(unsafe_state(
                    "spawned process group survived startup cleanup",
                ));
            }
            if sample < 4 {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        remove_registered_socket(&self.record)?;
        fs::remove_file(&self.path).map_err(|error| unsafe_state(error.to_string()))?;
        Ok(())
    }

    pub fn activate(&mut self, pid: u32) -> Result<(), MachineError> {
        let process = DarwinSystem
            .bsd_process_identity(pid)
            .map_err(|error| unsafe_state(error.to_string()))?;
        let live = DarwinSystem
            .live_process_identity(pid)
            .map_err(|error| unsafe_state(error.to_string()))?;
        let image_matches = DarwinSystem.live_process_path(pid).is_some_and(|path| {
            path == self.record.executable
                && fs::metadata(path).is_ok_and(|image| {
                    image.dev() == self.record.executable_device
                        && image.ino() == self.record.executable_inode
                })
        });
        if process.zombie
            || process.pid != process.process_group_id
            || process.session_id != pid
            || process.uid != DarwinSystem.current_uid()
            || live.process_group_id != pid
            || !live.fingerprint.contains(&self.record.expected_argument)
            || !image_matches
        {
            return Err(unsafe_state(
                "spawned process does not match its registration",
            ));
        }
        let socket_metadata = self
            .record
            .socket
            .as_ref()
            .map(|socket| crate::codex_socket::inspect(socket, process.uid))
            .transpose()
            .map_err(|error| unsafe_state(error.to_string()))?;
        if socket_metadata.as_ref().is_some_and(|metadata| {
            !metadata.file_type().is_socket() || metadata.uid() != process.uid
        }) {
            return Err(unsafe_state("registered socket is not a same-user socket"));
        }
        self.record.process = Some(process);
        self.record.fingerprint = Some(live.fingerprint);
        self.record.socket_device = socket_metadata.as_ref().map(|metadata| metadata.dev());
        self.record.socket_inode = socket_metadata.as_ref().map(|metadata| metadata.ino());
        write_record(&self.path, &self.record, true)?;
        self.activated = true;
        Ok(())
    }
}

fn read_record(path: &Path) -> Result<Record, MachineError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| unsafe_state(error.to_string()))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > MAX_RECORD_BYTES
    {
        return Err(unsafe_state("process inventory record is unsafe"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| unsafe_state(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unsafe_state(error.to_string()))?;
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|error| unsafe_state(error.to_string()))?;
    if record.schema_version != 1
        || path.file_name().and_then(|name| name.to_str()) != Some(&format!("{}.json", record.id))
        || !matches!(
            record.kind.as_str(),
            "profile_server" | "profile_log_drainer" | "dedicated_server" | "worker"
        )
        || record.owner_device == 0
        || record.owner_inode == 0
        || !record.owner_root.is_absolute()
        || !record.executable.is_absolute()
        || record.executable_device == 0
        || record.executable_inode == 0
        || record.expected_argument.is_empty()
        || record.process.is_some() != record.fingerprint.is_some()
        || record.socket_device.is_some() != record.socket_inode.is_some()
        || (record.socket.is_none() && record.socket_device.is_some())
        || record.process.is_none() && record.socket_device.is_some()
        || record.process.is_some_and(|process| {
            process.uid != DarwinSystem.current_uid()
                || process.zombie
                || process.pid != process.process_group_id
                || process.pid != process.session_id
        })
        || record
            .fingerprint
            .as_ref()
            .is_some_and(|fingerprint| !fingerprint.contains(&record.expected_argument))
        || record
            .socket
            .as_ref()
            .is_some_and(|socket| !socket.is_absolute())
    {
        return Err(unsafe_state("process inventory record identity is invalid"));
    }
    Ok(record)
}

fn classify(record: &Record) -> Verdict {
    if let Some(verdict) = classify_boot(&record.boot_uuid, DarwinSystem.boot_session_uuid()) {
        return verdict;
    }
    let owner_missing = match fs::symlink_metadata(&record.owner_root) {
        Ok(metadata) => {
            metadata.dev() != record.owner_device || metadata.ino() != record.owner_inode
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => return Verdict::Unverifiable,
    };
    let Some(process) = record.process else {
        // A crash between spawn and activation leaves no stable PID evidence.
        // A pathname search cannot prove absence after a test root is renamed.
        return Verdict::Unverifiable;
    };
    match DarwinSystem.bsd_process_identity(process.pid) {
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            return classify_remaining_group(&process, owner_missing);
        }
        Err(_) => return Verdict::Unverifiable,
        Ok(current) if !same_recorded_process(&current, &process) => {
            return Verdict::Unverifiable;
        }
        Ok(current) if current.zombie => {
            return classify_remaining_group(&process, owner_missing);
        }
        Ok(_) => {}
    }
    let Ok(live) = DarwinSystem.live_process_identity(process.pid) else {
        return Verdict::Unverifiable;
    };
    let executable_matches = match DarwinSystem.live_process_path(process.pid) {
        Some(path) => {
            path == record.executable
                || fs::metadata(path).is_ok_and(|image| {
                    image.file_type().is_file()
                        && image.dev() == record.executable_device
                        && image.ino() == record.executable_inode
                })
        }
        None => fs::symlink_metadata(&record.executable)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
    };
    // proc_pidpath may report a renamed image, the same path after a build
    // replaced its inode, or no path after unlink. The image was verified at
    // registration; stable BSD identity and the full ps fingerprint preserve
    // process continuity when the pathname no longer names its original inode.
    if Some(&live.fingerprint) != record.fingerprint.as_ref() || !executable_matches {
        return Verdict::Unverifiable;
    }
    if owner_missing {
        Verdict::Orphan
    } else {
        Verdict::Owned
    }
}

fn classify_boot(recorded: &str, current: std::io::Result<String>) -> Option<Verdict> {
    match current {
        Ok(current) if current == recorded => None,
        Ok(_) => Some(Verdict::Absent),
        Err(_) => Some(Verdict::Unverifiable),
    }
}

fn classify_remaining_group(process: &BsdProcessIdentity, owner_missing: bool) -> Verdict {
    match verify_group(process) {
        Ok(members) if members.is_empty() => Verdict::Absent,
        Ok(_) if owner_missing => Verdict::Orphan,
        Ok(_) => Verdict::Owned,
        Err(_) => Verdict::Unverifiable,
    }
}

fn same_recorded_process(current: &BsdProcessIdentity, recorded: &BsdProcessIdentity) -> bool {
    current.pid == recorded.pid
        && current.uid == recorded.uid
        && current.process_group_id == recorded.process_group_id
        && current.session_id == recorded.session_id
        && current.start_tvsec == recorded.start_tvsec
        && current.start_tvusec == recorded.start_tvusec
}

fn selection(arguments: &[OsString], cleanup: bool) -> Result<Selection, MachineError> {
    let mut result = Selection::default();
    let mut index = 0;
    while index < arguments.len() {
        let token = arguments[index]
            .to_str()
            .ok_or_else(|| MachineError::invalid_argument("argv", "non-UTF-8 selector"))?;
        let (flag, inline_value) = token
            .split_once('=')
            .map_or((token, None), |(flag, value)| (flag, Some(value)));
        if flag == "--all" && inline_value.is_none() {
            result.all = true;
            index += 1;
            continue;
        }
        let value = inline_value
            .or_else(|| arguments.get(index + 1).and_then(|value| value.to_str()))
            .ok_or_else(|| MachineError::invalid_argument(flag, "selector value is required"))?;
        match flag {
            "--owner-root" => {
                result.owner_roots.insert(PathBuf::from(value));
            }
            "--owner-root-under" => {
                result.owner_root_under.insert(PathBuf::from(value));
            }
            "--kind" => {
                result.kinds.insert(value.to_owned());
            }
            "--profile" => {
                result.profiles.insert(value.to_owned());
            }
            "--run-id" => {
                result.run_ids.insert(
                    Uuid::parse_str(value)
                        .map_err(|_| MachineError::invalid_argument(flag, "invalid UUID"))?,
                );
            }
            "--pid" => {
                result.pids.insert(
                    value
                        .parse()
                        .map_err(|_| MachineError::invalid_argument(flag, "invalid PID"))?,
                );
            }
            "--confirm-selection-sha256" if cleanup => result.confirm = Some(value.to_owned()),
            _ => {
                return Err(MachineError::invalid_argument(
                    flag,
                    "unsupported orphan selector",
                ));
            }
        }
        index += if inline_value.is_some() { 1 } else { 2 };
    }
    if cleanup && result.confirm.is_none() {
        return Err(MachineError::invalid_argument(
            "--confirm-selection-sha256",
            "confirmation digest is required",
        ));
    }
    if cleanup
        && !result.all
        && result.owner_roots.is_empty()
        && result.owner_root_under.is_empty()
        && result.kinds.is_empty()
        && result.profiles.is_empty()
        && result.run_ids.is_empty()
        && result.pids.is_empty()
    {
        return Err(MachineError::invalid_argument(
            "selectors",
            "cleanup requires a selector or --all",
        ));
    }
    for path in result
        .owner_roots
        .iter()
        .chain(result.owner_root_under.iter())
    {
        if !path.is_absolute() {
            return Err(MachineError::invalid_argument(
                "owner root",
                "absolute path required",
            ));
        }
    }
    Ok(result)
}

fn selected(record: &Record, selection: &Selection) -> bool {
    (selection.owner_roots.is_empty() || selection.owner_roots.contains(&record.owner_root))
        && (selection.owner_root_under.is_empty()
            || selection
                .owner_root_under
                .iter()
                .any(|path| record.owner_root.starts_with(path)))
        && (selection.kinds.is_empty() || selection.kinds.contains(&record.kind))
        && (selection.profiles.is_empty()
            || record
                .profile
                .as_ref()
                .is_some_and(|profile| selection.profiles.contains(profile)))
        && (selection.run_ids.is_empty()
            || record
                .run_id
                .is_some_and(|run| selection.run_ids.contains(&run)))
        && (selection.pids.is_empty()
            || record
                .process
                .is_some_and(|process| selection.pids.contains(&process.pid)))
}

fn inventory(selection: &Selection) -> Result<Vec<(PathBuf, Record, Candidate)>, MachineError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(root()?).map_err(|error| unsafe_state(error.to_string()))? {
        let entry = entry.map_err(|error| unsafe_state(error.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let record = read_record(&path)?;
        if !selected(&record, selection) {
            continue;
        }
        let verdict = classify(&record);
        let candidate = Candidate {
            id: record.id,
            kind: record.kind.clone(),
            owner_root: record.owner_root.clone(),
            profile: record.profile.clone(),
            run_id: record.run_id,
            pid: record.process.map(|process| process.pid),
            process_group_id: record.process.map(|process| process.process_group_id),
            socket: record.socket.clone(),
            verdict,
        };
        entries.push((path, record, candidate));
    }
    entries.sort_by_key(|(_, record, _)| record.id);
    Ok(entries)
}

fn digest(entries: &[(PathBuf, Record, Candidate)]) -> Result<String, MachineError> {
    let evidence = entries.iter().map(|(_, record, candidate)| json!({
        "id": record.id, "boot_uuid": record.boot_uuid, "owner_device": record.owner_device,
        "owner_inode": record.owner_inode, "executable_device": record.executable_device,
        "executable_inode": record.executable_inode, "process": record.process, "fingerprint": record.fingerprint,
        "socket_device": record.socket_device, "socket_inode": record.socket_inode,
        "verdict": candidate.verdict,
    })).collect::<Vec<_>>();
    let source =
        serde_json::to_string(&evidence).map_err(|error| unsafe_state(error.to_string()))?;
    let value = crate::jcs::parse(&source).map_err(|error| unsafe_state(error.to_string()))?;
    let bytes =
        crate::jcs::canonicalize(&value).map_err(|error| unsafe_state(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn cleanup_one(path: &Path, record: &Record) -> Result<(), MachineError> {
    match classify(record) {
        Verdict::Orphan => {}
        Verdict::Absent | Verdict::Owned => {
            return Err(changed_selection("candidate changed before signal"));
        }
        Verdict::Unverifiable => return Err(unsafe_state("candidate changed before signal")),
    }
    let process = record.process.expect("orphan has process");
    match verify_group(&process) {
        Ok(members) if members.is_empty() => {
            return Err(changed_selection("candidate exited before signal"));
        }
        Ok(_) => {}
        Err(error) => {
            return if classify(record) == Verdict::Absent {
                Err(changed_selection("candidate exited before signal"))
            } else {
                Err(error)
            };
        }
    }
    DarwinSystem
        .signal_process_group(process.process_group_id, libc::SIGTERM)
        .map_err(|error| unsafe_state(error.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if verify_group(&process)?.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !verify_group(&process)?.is_empty() {
        match classify(record) {
            Verdict::Orphan => {}
            Verdict::Absent | Verdict::Owned => {
                return Err(changed_selection("candidate changed before force"));
            }
            Verdict::Unverifiable => return Err(unsafe_state("candidate changed before force")),
        }
        if verify_group(&process)?.is_empty() {
            return Err(changed_selection("candidate exited before force"));
        }
        DarwinSystem
            .signal_process_group(process.process_group_id, libc::SIGKILL)
            .map_err(|error| unsafe_state(error.to_string()))?;
        let kill_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < kill_deadline && !verify_group(&process)?.is_empty() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    for sample in 0..5 {
        if !verify_group(&process)?.is_empty() {
            return Err(unsafe_state("process group survived cleanup"));
        }
        if sample < 4 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    remove_registered_socket(record)?;
    fs::remove_file(path).map_err(|error| unsafe_state(error.to_string()))?;
    Ok(())
}

fn remove_registered_socket(record: &Record) -> Result<(), MachineError> {
    if let (Some(socket), Some(device), Some(inode), Some(process)) = (
        &record.socket,
        record.socket_device,
        record.socket_inode,
        record.process,
    ) {
        crate::codex_socket::remove_recorded(socket, process.uid, device, inode)
            .map_err(|error| unsafe_state(error.to_string()))?;
    }
    Ok(())
}

fn verify_group(process: &BsdProcessIdentity) -> Result<Vec<u32>, MachineError> {
    match DarwinSystem.bsd_process_identity(process.pid) {
        Ok(leader) if same_recorded_process(&leader, process) => {}
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
        _ => return Err(unsafe_state("process group leader changed")),
    }
    let census = DarwinSystem
        .process_group_pids(process.process_group_id)
        .map_err(|error| unsafe_state(error.to_string()))?;
    let mut members = Vec::new();
    for pid in census {
        let member = match DarwinSystem.bsd_process_identity(pid) {
            Ok(member) if !member.zombie => member,
            Ok(_) => continue,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => continue,
            Err(error) => return Err(unsafe_state(error.to_string())),
        };
        if member.uid != process.uid
            || member.session_id != process.session_id
            || member.process_group_id != process.process_group_id
        {
            return Err(unsafe_state("unverified group member"));
        }
        members.push(pid);
    }
    Ok(members)
}

pub fn execute(cleanup: bool, arguments: &[OsString]) -> Result<Value, MachineError> {
    let selection = selection(arguments, cleanup)?;
    let entries = inventory(&selection)?;
    let selection_sha256 = digest(&entries)?;
    if cleanup && selection.confirm.as_deref() != Some(&selection_sha256) {
        return Err(MachineError::new(
            "ORPHAN_SELECTION_CHANGED",
            "orphan selection changed after inspection",
            true,
            json!({"selection_sha256": selection_sha256}),
        ));
    }
    let candidates = entries
        .iter()
        .map(|(_, _, candidate)| candidate)
        .collect::<Vec<_>>();
    if !cleanup {
        return Ok(json!({"selection_sha256": selection_sha256, "candidates": candidates}));
    }
    let mut cleaned = Vec::new();
    let mut retired = Vec::new();
    let mut skipped = Vec::new();
    for (path, record, candidate) in &entries {
        if candidate.verdict == Verdict::Orphan {
            cleanup_one(path, record)?;
            cleaned.push(candidate.id);
        } else if candidate.verdict == Verdict::Absent && classify(record) == Verdict::Absent {
            remove_registered_socket(record)?;
            fs::remove_file(path).map_err(|error| unsafe_state(error.to_string()))?;
            retired.push(candidate.id);
        } else {
            skipped.push(candidate.id);
        }
    }
    Ok(
        json!({"selection_sha256": selection_sha256, "cleaned": cleaned, "retired": retired, "skipped": skipped}),
    )
}

/// Retire only an exact recorded PID after ordinary lifecycle absence proof.
pub fn retire_pid(pid: u32) -> Result<(), MachineError> {
    for entry in fs::read_dir(root()?).map_err(|error| unsafe_state(error.to_string()))? {
        let path = entry
            .map_err(|error| unsafe_state(error.to_string()))?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let record = read_record(&path)?;
        if let Some(process) = record.process.filter(|process| process.pid == pid) {
            match DarwinSystem.bsd_process_identity(pid) {
                Ok(current) if !same_recorded_process(&current, &process) => {
                    // A previous registration may still name a PID now used
                    // by a different generation. It does not own this exit.
                    continue;
                }
                Ok(current) if current.zombie => {}
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
                _ => return Err(unsafe_state("cannot retire live process registration")),
            }
            let members = DarwinSystem
                .process_group_pids(pid)
                .map_err(|error| unsafe_state(error.to_string()))?;
            if members.iter().any(|member| *member != pid) {
                return Err(unsafe_state("cannot retire live process registration"));
            }
            remove_registered_socket(&record)?;
            fs::remove_file(path).map_err(|error| unsafe_state(error.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    struct ChildCleanup(Option<Child>);

    impl Drop for ChildCleanup {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[test]
    fn boot_lookup_failure_is_not_process_absence() {
        assert_eq!(classify_boot("boot-a", Ok("boot-a".to_owned())), None);
        assert_eq!(
            classify_boot("boot-a", Ok("boot-b".to_owned())),
            Some(Verdict::Absent)
        );
        assert_eq!(
            classify_boot("boot-a", Err(std::io::Error::other("boot lookup failed"))),
            Some(Verdict::Unverifiable)
        );
    }

    #[test]
    fn removed_owner_requires_digest_and_exact_process_before_cleanup() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-orphan-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut registration = Registration::prepare(
            "profile_log_drainer",
            &owner,
            Path::new("/bin/sleep"),
            "sleep 30".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let mut child = ChildCleanup(Some(DarwinSystem.spawn_detached(&mut command).unwrap()));
        let pid = child.0.as_ref().unwrap().id();
        {
            registration.activate(pid).unwrap();
            let mut changed_identity = registration.record.clone();
            changed_identity.process.as_mut().unwrap().start_tvusec ^= 1;
            assert_eq!(classify(&changed_identity), Verdict::Unverifiable);
            let selectors = [
                OsString::from("--owner-root"),
                owner.as_os_str().to_os_string(),
            ];
            let owned = execute(false, &selectors).unwrap();
            assert_eq!(owned["candidates"][0]["verdict"], "owned");
            fs::remove_dir(&owner).unwrap();
            let orphan = execute(false, &selectors).unwrap();
            assert_eq!(orphan["candidates"][0]["verdict"], "orphan");
            let stale = [
                selectors[0].clone(),
                selectors[1].clone(),
                OsString::from("--confirm-selection-sha256"),
                OsString::from("0".repeat(64)),
            ];
            assert_eq!(
                execute(true, &stale).unwrap_err().code,
                "ORPHAN_SELECTION_CHANGED"
            );
            let valid = [
                selectors[0].clone(),
                selectors[1].clone(),
                OsString::from("--confirm-selection-sha256"),
                OsString::from(orphan["selection_sha256"].as_str().unwrap()),
            ];
            let mut waited_child = child.0.take().unwrap();
            let reaper = std::thread::spawn(move || waited_child.wait().unwrap());
            let cleaned = execute(true, &valid);
            if cleaned.is_err() {
                let _ = DarwinSystem.signal_process_group(pid, libc::SIGKILL);
            }
            reaper.join().unwrap();
            let cleaned = cleaned.unwrap();
            assert_eq!(cleaned["cleaned"].as_array().unwrap().len(), 1);
            assert!(
                execute(false, &selectors).unwrap()["candidates"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn normal_shutdown_retires_a_verified_exited_leader() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-zombie-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut registration = Registration::prepare(
            "profile_log_drainer",
            &owner,
            Path::new("/bin/sleep"),
            "sleep 30".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let mut child = ChildCleanup(Some(DarwinSystem.spawn_detached(&mut command).unwrap()));
        let pid = child.0.as_ref().unwrap().id();
        registration.activate(pid).unwrap();
        DarwinSystem
            .signal_process_group(pid, libc::SIGTERM)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while DarwinSystem
            .bsd_process_identity(pid)
            .is_ok_and(|process| !process.zombie)
        {
            assert!(Instant::now() < deadline, "child did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        retire_pid(pid).unwrap();
        child.0.take().unwrap().wait().unwrap();
        fs::remove_dir(owner).unwrap();
    }

    #[test]
    fn retiring_an_absent_server_removes_its_recorded_socket() {
        let owner = fs::canonicalize("/tmp")
            .unwrap()
            .join(format!("dolgorae-socket-retire-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let socket = owner.join("server.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut registration = Registration::prepare(
            "profile_server",
            &owner,
            Path::new("/bin/sleep"),
            "sleep 30".to_owned(),
            Some(&socket),
            None,
            None,
        )
        .unwrap();
        let mut child = ChildCleanup(Some(DarwinSystem.spawn_detached(&mut command).unwrap()));
        let pid = child.0.as_ref().unwrap().id();
        registration.activate(pid).unwrap();
        drop(listener);
        DarwinSystem
            .signal_process_group(pid, libc::SIGTERM)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while DarwinSystem
            .bsd_process_identity(pid)
            .is_ok_and(|process| !process.zombie)
        {
            assert!(Instant::now() < deadline, "child did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        retire_pid(pid).unwrap();
        assert!(!socket.exists());
        assert!(!registration.path.exists());
        child.0.take().unwrap().wait().unwrap();
        fs::remove_dir(owner).unwrap();
    }

    #[test]
    fn provisional_registration_is_not_mistaken_for_an_absent_process() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-provisional-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let registration = Registration::prepare(
            "profile_server",
            &owner,
            Path::new("/bin/sleep"),
            "sleep 30".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(registration.path.exists());
        assert_eq!(classify(&registration.record), Verdict::Unverifiable);
        let path = registration.path.clone();
        drop(registration);
        assert!(!path.exists());
        fs::remove_dir(owner).unwrap();
    }

    #[test]
    fn failed_start_keeps_registration_if_group_identity_changes() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-abort-identity-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let mut registration = Registration::prepare(
            "profile_server",
            &owner,
            Path::new("/bin/sleep"),
            "sleep 30".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = ChildCleanup(Some(DarwinSystem.spawn_detached(&mut command).unwrap()));
        registration.mark_spawned(child.0.as_ref().unwrap().id());
        registration.spawned_process.as_mut().unwrap().start_tvusec ^= 1;
        assert!(registration.abort_spawn().is_err());
        let path = registration.path.clone();
        drop(registration);
        assert!(path.exists());
        fs::remove_file(path).unwrap();
        let mut stopped = child.0.take().unwrap();
        stopped.kill().unwrap();
        stopped.wait().unwrap();
        fs::remove_dir(owner).unwrap();
    }

    #[test]
    fn failed_start_stops_group_children_before_retiring_registration() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-abort-group-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let script = "trap 'exit 0' TERM; (trap '' TERM; exec sleep 30) & wait";
        let mut command = Command::new("/bin/bash");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut registration = Registration::prepare(
            "profile_server",
            &owner,
            Path::new("/bin/bash"),
            "-c".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let mut child = ChildCleanup(Some(DarwinSystem.spawn_detached(&mut command).unwrap()));
        let pid = child.0.as_ref().unwrap().id();
        registration.mark_spawned(pid);
        let process = registration.spawned_process.unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while verify_group(&process).unwrap().len() < 2 {
            assert!(Instant::now() < deadline, "group child did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        let result = registration.abort_spawn();
        if result.is_err() {
            let _ = DarwinSystem.signal_process_group(pid, libc::SIGKILL);
        }
        child.0.take().unwrap().wait().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(verify_group(&process).unwrap().is_empty());
        assert!(!registration.path.exists());
        fs::remove_dir(owner).unwrap();
    }

    #[test]
    fn failed_start_reaps_group_after_leader_exits_before_socket_bind() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-early-exit-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let mut command = Command::new("/bin/bash");
        command
            .args(["-c", "(trap '' TERM; exec sleep 30) & sleep 0.3"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut registration = Registration::prepare(
            "dedicated_server",
            &owner,
            Path::new("/bin/bash"),
            "-c".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let mut child = ChildCleanup(Some(DarwinSystem.spawn_detached(&mut command).unwrap()));
        let pid = child.0.as_ref().unwrap().id();
        registration.mark_spawned(pid);
        let process = registration.spawned_process.unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "group leader did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!verify_group(&process).unwrap().is_empty());
        assert_eq!(classify(&registration.record), Verdict::Unverifiable);
        let result = registration.abort_spawn();
        if result.is_err() {
            let _ = DarwinSystem.signal_process_group(pid, libc::SIGKILL);
        }
        child.0.take().unwrap().wait().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(verify_group(&process).unwrap().is_empty());
        assert!(!registration.path.exists());
        fs::remove_dir(owner).unwrap();
    }

    #[test]
    fn interrupted_provisional_registration_remains_unverifiable_after_owner_move() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-interrupted-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let registration = Registration::prepare(
            "profile_server",
            &owner,
            Path::new("/bin/sleep"),
            "sleep 30".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let record = registration.record.clone();
        let record_path = registration.path.clone();
        std::mem::forget(registration); // Simulate the parent dying before activation.
        let retired = owner.with_extension("retired");
        fs::rename(&owner, &retired).unwrap();
        assert_eq!(classify(&record), Verdict::Unverifiable);
        fs::remove_file(record_path).unwrap();
        fs::remove_dir(retired).unwrap();
    }

    #[test]
    fn pid_retirement_ignores_an_older_generation() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-pid-reuse-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut registration = Registration::prepare(
            "profile_log_drainer",
            &owner,
            Path::new("/bin/sleep"),
            "sleep 30".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let mut child = ChildCleanup(Some(DarwinSystem.spawn_detached(&mut command).unwrap()));
        let pid = child.0.as_ref().unwrap().id();
        registration.activate(pid).unwrap();
        let mut previous = registration.record.clone();
        previous.id = Uuid::now_v7();
        previous.process.as_mut().unwrap().start_tvusec ^= 1;
        let previous_path = root().unwrap().join(format!("{}.json", previous.id));
        write_record(&previous_path, &previous, false).unwrap();
        DarwinSystem
            .signal_process_group(pid, libc::SIGTERM)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while DarwinSystem
            .bsd_process_identity(pid)
            .is_ok_and(|process| !process.zombie)
        {
            assert!(Instant::now() < deadline, "child did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        retire_pid(pid).unwrap();
        assert!(!registration.path.exists());
        if previous_path.exists() {
            fs::remove_file(previous_path).unwrap();
        }
        child.0.take().unwrap().wait().unwrap();
        fs::remove_dir(owner).unwrap();
    }

    #[test]
    fn orphan_cleanup_reaps_group_after_leader_exits() {
        let owner = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dolgorae-group-test-{}", Uuid::now_v7()));
        fs::create_dir(&owner).unwrap();
        let script = "trap 'exit 0' TERM; (trap '' TERM; exec sleep 30) & wait";
        let mut command = Command::new("/bin/bash");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut registration = Registration::prepare(
            "profile_server",
            &owner,
            Path::new("/bin/bash"),
            "-c".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let mut child = ChildCleanup(Some(DarwinSystem.spawn_detached(&mut command).unwrap()));
        let pid = child.0.as_ref().unwrap().id();
        registration.activate(pid).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while DarwinSystem.process_group_pids(pid).unwrap().len() < 2 {
            assert!(Instant::now() < deadline, "group child did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        fs::remove_dir(&owner).unwrap();
        let selectors = [OsString::from("--owner-root"), owner.into_os_string()];
        let inspected = execute(false, &selectors).unwrap();
        let cleanup = [
            selectors[0].clone(),
            selectors[1].clone(),
            OsString::from("--confirm-selection-sha256"),
            OsString::from(inspected["selection_sha256"].as_str().unwrap()),
        ];
        let result = execute(true, &cleanup);
        if result.is_err() {
            let _ = DarwinSystem.signal_process_group(pid, libc::SIGKILL);
        }
        child.0.take().unwrap().wait().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(
            verify_group(&registration.record.process.unwrap())
                .unwrap()
                .is_empty()
        );
    }
}
