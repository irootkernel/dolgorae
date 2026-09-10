//! Installation singleton and descriptor-relative ownership of the public UDS.
use crate::darwin::{AtNodeIdentity, DarwinSystem};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const RECORD: &str = "gateway.json";
const LOCK: &str = "gateway.lock";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayRecord {
    pub schema_version: u32,
    pub boot_uuid: String,
    pub pid: u32,
    pub uid: u32,
    pub start_tvsec: u64,
    pub start_tvusec: u64,
    pub binary_digest: String,
    pub socket_path: PathBuf,
    pub socket_device: u64,
    pub socket_inode: u64,
    pub server_instance_id: Uuid,
    pub protocol_min: u32,
    pub protocol_max: u32,
}

/// Keep this guard alive until all accepted calls and streams have drained.
/// Moving its listener into an async runtime does not transfer node ownership.
pub struct GatewaySocket {
    listener: Option<UnixListener>,
    parent: File,
    name: OsString,
    rpc: File,
    _lock: File,
    record: GatewayRecord,
}

impl GatewaySocket {
    pub fn bind(home: &DolgoraeHome, socket_path: &Path) -> Result<Self, MachineError> {
        let system = DarwinSystem;
        let uid = system.current_uid();
        let (parent, name) = socket_parent(socket_path, uid)?;
        let home_parent = open_directory(
            home.root()
                .parent()
                .ok_or_else(|| unsafe_socket("home has no parent"))?,
        )?;
        ensure_directory(&home_parent, OsStr::new(".dolgorae"), uid)?;
        let home_directory = system
            .openat_nofollow(&home_parent, OsStr::new(".dolgorae"), true)
            .map_err(io_error)?;
        ensure_directory(&home_directory, OsStr::new("rpc"), uid)?;
        let rpc = system
            .openat_nofollow(&home_directory, OsStr::new("rpc"), true)
            .map_err(io_error)?;
        validate_directory(&home_directory, uid)?;
        validate_directory(&rpc, uid)?;
        let lock = system
            .createat_private(&rpc, OsStr::new(LOCK), false)
            .map_err(io_error)?;
        validate_file(&lock, uid)?;
        let prior = read_record(&rpc, uid)?;
        match system.lock_exclusive_nonblocking(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(MachineError::new(
                    "RPC_SERVER_ALREADY_RUNNING",
                    "a gateway already holds the installation lock",
                    false,
                    json!({
                        "server_instance_id": prior.as_ref().map(|record| record.server_instance_id),
                        "socket_path": prior.as_ref().map_or(socket_path, |record| record.socket_path.as_path()),
                        "required_action": "connect to the existing gateway or wait for its supervised shutdown"
                    }),
                ));
            }
            Err(error) => return Err(io_error(error)),
        }
        require_file_identity(&rpc, OsStr::new(LOCK), &lock)?;
        // Re-read after lock acquisition; the prior holder may have published
        // its record between our first read and acquisition.
        let prior = read_record(&rpc, uid)?;
        let boot_uuid = system.boot_session_uuid().map_err(io_error)?;
        if let Some(prior) = &prior {
            prove_absent(prior, &boot_uuid, uid)?;
        }
        if let Some(node) = optional_node(&parent, &name)? {
            let prior = prior
                .as_ref()
                .ok_or_else(|| unsafe_socket("existing socket has no exact gateway record"))?;
            if prior.socket_path != socket_path
                || prior.socket_device != node.device
                || prior.socket_inode != node.inode
            {
                return Err(unsafe_socket(
                    "existing node does not match the prior gateway record",
                ));
            }
            validate_socket(node, uid)?;
            unlink_matching(&parent, &name, node.device, node.inode)?;
        }
        // Re-open the complete absolute path before binding, preserving the
        // no-follow traversal and detecting a renamed parent.
        validate_parent_identity(socket_path, &parent, uid)?;
        let process = system
            .bsd_process_identity(std::process::id())
            .map_err(io_error)?;
        let binary_digest = binary_digest()?;
        let listener = system.bind_unix_at(&parent, &name).map_err(io_error)?;
        let node = system.statat_nofollow(&parent, &name).map_err(io_error)?;
        let record = GatewayRecord {
            schema_version: 1,
            boot_uuid,
            pid: process.pid,
            uid,
            start_tvsec: process.start_tvsec,
            start_tvusec: process.start_tvusec,
            binary_digest,
            socket_path: socket_path.to_path_buf(),
            socket_device: node.device,
            socket_inode: node.inode,
            server_instance_id: Uuid::now_v7(),
            protocol_min: 1,
            protocol_max: 1,
        };
        let guard = Self {
            listener: Some(listener),
            parent,
            name,
            rpc,
            _lock: lock,
            record,
        };
        system
            .chmodat_private_socket(&guard.parent, &guard.name)
            .map_err(io_error)?;
        let ready_node = system
            .statat_nofollow(&guard.parent, &guard.name)
            .map_err(io_error)?;
        validate_socket(ready_node, uid)?;
        if ready_node.device != node.device || ready_node.inode != node.inode {
            return Err(unsafe_socket("socket inode changed during startup"));
        }
        validate_parent_identity(socket_path, &guard.parent, uid)?;
        require_file_identity(&guard.rpc, OsStr::new(LOCK), &guard._lock)?;
        let current_rpc = open_directory(&home.root().join("rpc"))?;
        validate_directory(&current_rpc, uid)?;
        let current_rpc = current_rpc.metadata().map_err(io_error)?;
        let retained_rpc = guard.rpc.metadata().map_err(io_error)?;
        if current_rpc.dev() != retained_rpc.dev() || current_rpc.ino() != retained_rpc.ino() {
            return Err(unsafe_socket("gateway authority directory was replaced"));
        }
        guard.persist_record()?;
        Ok(guard)
    }

    pub fn take_listener(&mut self) -> Option<UnixListener> {
        self.listener.take()
    }

    pub fn record(&self) -> &GatewayRecord {
        &self.record
    }

    pub fn readiness_data(&self) -> Value {
        json!({ "server_instance_id": self.record.server_instance_id,
            "socket_path": self.record.socket_path, "rpc_protocol_version": 1,
            "minimum_client_version": self.record.protocol_min,
            "maximum_client_version": self.record.protocol_max,
            "descriptor_sha256": crate::protocol::PUBLIC_V1_DESCRIPTOR_SHA256 })
    }

    pub fn verify_peer(socket: &UnixStream) -> Result<(), MachineError> {
        verify_peer_uid(
            DarwinSystem.peer_uid(socket).map_err(io_error)?,
            DarwinSystem.current_uid(),
        )
    }

    /// Explicit cleanup reports failures; Drop provides the same conservative
    /// fallback on startup errors. The singleton record remains for stale proof.
    pub fn cleanup(&mut self) -> Result<(), MachineError> {
        self.listener.take();
        validate_directory(&self.parent, self.record.uid)?;
        unlink_matching(
            &self.parent,
            &self.name,
            self.record.socket_device,
            self.record.socket_inode,
        )
    }

    fn persist_record(&self) -> Result<(), MachineError> {
        let temporary = OsString::from(format!("gateway-{}.tmp", Uuid::now_v7()));
        let result = (|| {
            let mut file = DarwinSystem
                .createat_private(&self.rpc, &temporary, true)
                .map_err(io_error)?;
            let bytes = serde_json::to_vec(&self.record)
                .map_err(|error| unsafe_socket(error.to_string()))?;
            file.write_all(&bytes).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            DarwinSystem
                .renameat_file(&self.rpc, &temporary, OsStr::new(RECORD))
                .map_err(io_error)?;
            self.rpc.sync_all().map_err(io_error)
        })();
        if result.is_err() {
            let _ = DarwinSystem.unlinkat_file(&self.rpc, &temporary);
        }
        result
    }
}

impl Drop for GatewaySocket {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn open_directory(path: &Path) -> Result<File, MachineError> {
    if !path.is_absolute() {
        return Err(unsafe_socket("path must be absolute"));
    }
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(io_error)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = DarwinSystem
                    .openat_nofollow(&directory, name, true)
                    .map_err(io_error)?
            }
            _ => return Err(unsafe_socket("path contains a non-normal component")),
        }
    }
    Ok(directory)
}

fn socket_parent(path: &Path, uid: u32) -> Result<(File, OsString), MachineError> {
    // Darwin sockaddr_un has 104 sun_path bytes including the terminating NUL.
    if path.as_os_str().as_bytes().len() >= 104 {
        return Err(unsafe_socket(
            "absolute socket path exceeds the Darwin Unix socket limit",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| unsafe_socket("socket path has no filename"))?
        .to_owned();
    let parent = open_directory(
        path.parent()
            .ok_or_else(|| unsafe_socket("socket has no parent"))?,
    )?;
    validate_directory(&parent, uid)?;
    Ok((parent, name))
}

fn validate_directory(file: &File, uid: u32) -> Result<(), MachineError> {
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
        return Err(unsafe_socket(
            "directory must be current-uid-owned with mode 0700",
        ));
    }
    Ok(())
}

fn ensure_directory(parent: &File, name: &OsStr, uid: u32) -> Result<(), MachineError> {
    match DarwinSystem.mkdirat_private(parent, name) {
        Ok(()) => parent.sync_all().map_err(io_error)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_error(error)),
    }
    let directory = DarwinSystem
        .openat_nofollow(parent, name, true)
        .map_err(io_error)?;
    validate_directory(&directory, uid)
}

fn validate_file(file: &File, uid: u32) -> Result<(), MachineError> {
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(unsafe_socket(
            "gateway authority must be a private regular file with one link",
        ));
    }
    Ok(())
}

fn require_file_identity(directory: &File, name: &OsStr, file: &File) -> Result<(), MachineError> {
    let node = DarwinSystem
        .statat_nofollow(directory, name)
        .map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if node.device != metadata.dev() || node.inode != metadata.ino() {
        return Err(unsafe_socket("gateway lock inode was replaced"));
    }
    Ok(())
}

fn validate_parent_identity(path: &Path, parent: &File, uid: u32) -> Result<(), MachineError> {
    let (current, _) = socket_parent(path, uid)?;
    let current = current.metadata().map_err(io_error)?;
    let bound = parent.metadata().map_err(io_error)?;
    if current.dev() != bound.dev() || current.ino() != bound.ino() {
        return Err(unsafe_socket("socket parent identity changed"));
    }
    Ok(())
}

fn read_record(directory: &File, uid: u32) -> Result<Option<GatewayRecord>, MachineError> {
    let file = match DarwinSystem.openat_nofollow(directory, OsStr::new(RECORD), false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    validate_file(&file, uid)?;
    let mut bytes = Vec::new();
    file.take(16_385)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > 16_384 {
        return Err(unsafe_socket("gateway record exceeds its size bound"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| unsafe_socket("gateway record is malformed"))
}

fn prove_absent(record: &GatewayRecord, boot_uuid: &str, uid: u32) -> Result<(), MachineError> {
    if record.schema_version != 1
        || record.uid != uid
        || record.pid == 0
        || Uuid::parse_str(&record.boot_uuid).is_err()
        || !record.socket_path.is_absolute()
        || record.binary_digest.len() != 64
        || !record
            .binary_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || record.server_instance_id.get_version_num() != 7
        || record.protocol_min != 1
        || record.protocol_max != 1
    {
        return Err(unsafe_socket("gateway record identity is invalid"));
    }
    if record.boot_uuid != boot_uuid {
        return Ok(());
    }
    match DarwinSystem.bsd_process_identity(record.pid) {
        Ok(process)
            if process.start_tvsec != record.start_tvsec
                || process.start_tvusec != record.start_tvusec =>
        {
            Ok(())
        }
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        Ok(_) => Err(unsafe_socket("prior gateway process is still present")),
        Err(_) => Err(unsafe_socket(
            "prior gateway process absence cannot be proved",
        )),
    }
}

fn optional_node(directory: &File, name: &OsStr) -> Result<Option<AtNodeIdentity>, MachineError> {
    match DarwinSystem.statat_nofollow(directory, name) {
        Ok(node) => Ok(Some(node)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

fn validate_socket(node: AtNodeIdentity, uid: u32) -> Result<(), MachineError> {
    if node.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFSOCK)
        || node.uid != uid
        || node.mode & 0o7777 != 0o600
        || node.links != 1
    {
        return Err(unsafe_socket(
            "existing node must be a private current-uid-owned socket",
        ));
    }
    Ok(())
}

fn unlink_matching(
    directory: &File,
    name: &OsStr,
    device: u64,
    inode: u64,
) -> Result<(), MachineError> {
    if let Some(node) = optional_node(directory, name)? {
        if node.device != device
            || node.inode != inode
            || node.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFSOCK)
        {
            return Err(unsafe_socket(
                "socket node was replaced; replacement is preserved",
            ));
        }
        DarwinSystem
            .unlinkat_file(directory, name)
            .map_err(io_error)?;
        directory.sync_all().map_err(io_error)?;
    }
    Ok(())
}

fn binary_digest() -> Result<String, MachineError> {
    let mut file = File::open(std::env::current_exe().map_err(io_error)?).map_err(io_error)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65_536];
    loop {
        let count = file.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn io_error(error: std::io::Error) -> MachineError {
    unsafe_socket(error.to_string())
}
fn unsafe_socket(reason: impl Into<String>) -> MachineError {
    MachineError::new(
        "RPC_SOCKET_UNSAFE",
        "gateway socket ownership could not be verified",
        false,
        json!({ "reason": reason.into(), "required_action": "fix or replace the private socket parent/path before retrying" }),
    )
}

fn verify_peer_uid(peer_uid: u32, gateway_uid: u32) -> Result<(), MachineError> {
    if peer_uid != gateway_uid {
        return Err(unsafe_socket(
            "connection peer uid does not match the gateway uid",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    struct Fixture {
        root: PathBuf,
        home: DolgoraeHome,
        socket: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("gw-{}", Uuid::now_v7().simple()));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let socket = root.join("g.sock");
            let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
            Self { root, home, socket }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn binds_private_socket_without_changing_process_directory_and_checks_peer() {
        let fixture = Fixture::new();
        let cwd = std::env::current_dir().unwrap();
        let mut gateway = GatewaySocket::bind(&fixture.home, &fixture.socket).unwrap();
        assert_eq!(std::env::current_dir().unwrap(), cwd);
        assert_eq!(
            fs::symlink_metadata(&fixture.socket).unwrap().mode() & 0o7777,
            0o600
        );
        let client = UnixStream::connect(&fixture.socket).unwrap();
        let listener = gateway.take_listener().unwrap();
        let (connection, _) = listener.accept().unwrap();
        GatewaySocket::verify_peer(&connection).unwrap();
        GatewaySocket::verify_peer(&client).unwrap();
        gateway.cleanup().unwrap();
        assert!(!fixture.socket.exists());
    }

    #[test]
    fn peer_uid_decision_rejects_a_foreign_account() {
        verify_peer_uid(501, 501).unwrap();
        let error = verify_peer_uid(502, 501).unwrap_err();
        assert_eq!(error.code, "RPC_SOCKET_UNSAFE");
        assert_eq!(
            error.details["reason"],
            "connection peer uid does not match the gateway uid"
        );
    }

    #[test]
    fn installation_lock_prevents_second_socket() {
        let fixture = Fixture::new();
        let _gateway = GatewaySocket::bind(&fixture.home, &fixture.socket).unwrap();
        let second = fixture.root.join("s.sock");
        let error = GatewaySocket::bind(&fixture.home, &second).err().unwrap();
        assert_eq!(error.code, "RPC_SERVER_ALREADY_RUNNING");
        assert!(!second.exists());
    }

    #[test]
    fn rejects_unsafe_parent_symlink_and_unrecorded_socket() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            GatewaySocket::bind(&fixture.home, &fixture.socket)
                .err()
                .unwrap()
                .code,
            "RPC_SOCKET_UNSAFE"
        );
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = fixture.root.join("alias");
        symlink(&fixture.root, &alias).unwrap();
        assert!(GatewaySocket::bind(&fixture.home, &alias.join("g.sock")).is_err());
        let _foreign = UnixListener::bind(&fixture.socket).unwrap();
        assert!(GatewaySocket::bind(&fixture.home, &fixture.socket).is_err());
        assert!(fixture.socket.exists());
    }

    #[test]
    fn cleanup_preserves_replacement_node() {
        let fixture = Fixture::new();
        let mut gateway = GatewaySocket::bind(&fixture.home, &fixture.socket).unwrap();
        fs::remove_file(&fixture.socket).unwrap();
        fs::write(&fixture.socket, b"replacement").unwrap();
        assert_eq!(gateway.cleanup().unwrap_err().code, "RPC_SOCKET_UNSAFE");
        drop(gateway);
        assert_eq!(fs::read(&fixture.socket).unwrap(), b"replacement");
    }

    #[test]
    fn a_live_record_cannot_be_taken_over_even_after_lock_release() {
        let fixture = Fixture::new();
        let gateway = GatewaySocket::bind(&fixture.home, &fixture.socket).unwrap();
        drop(gateway);
        let error = GatewaySocket::bind(&fixture.home, &fixture.socket)
            .err()
            .unwrap();
        assert_eq!(error.code, "RPC_SOCKET_UNSAFE");
        assert!(!fixture.socket.exists());
    }

    #[test]
    fn stale_socket_requires_exact_record_and_absent_process() {
        let fixture = Fixture::new();
        let gateway = GatewaySocket::bind(&fixture.home, &fixture.socket).unwrap();
        let mut record = gateway.record().clone();
        // Keep the socket but release the lock, simulating a killed owner.
        let listener = gateway.listener.as_ref().unwrap().try_clone().unwrap();
        drop(gateway);
        drop(listener);
        let stale = UnixListener::bind(&fixture.socket).unwrap();
        fs::set_permissions(&fixture.socket, fs::Permissions::from_mode(0o600)).unwrap();
        let node = fs::symlink_metadata(&fixture.socket).unwrap();
        record.socket_device = node.dev();
        record.socket_inode = node.ino();
        record.boot_uuid = Uuid::now_v7().to_string();
        let record_path = fixture.home.root().join("rpc/gateway.json");
        fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        drop(stale);
        let replacement = GatewaySocket::bind(&fixture.home, &fixture.socket).unwrap();
        assert_ne!(
            replacement.record().server_instance_id,
            record.server_instance_id
        );
    }
}
