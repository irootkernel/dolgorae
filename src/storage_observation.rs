//! Checked short-lived SQLite leases and durable-commit wakeups.
use crate::machine::MachineError;
use rusqlite::{Connection, OpenFlags, Transaction};
use std::collections::BTreeMap;
use std::fs;
use std::ops::{Deref, DerefMut};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

static STATEMENTS: crate::darwin::SqliteStatementCounters =
    crate::darwin::SqliteStatementCounters::new();
static MIGRATIONS: AtomicU64 = AtomicU64::new(0);
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);
static OPENS: AtomicU64 = AtomicU64::new(0);
static REUSES: AtomicU64 = AtomicU64::new(0);
static INVALIDATIONS: AtomicU64 = AtomicU64::new(0);
static WAITS: AtomicU64 = AtomicU64::new(0);
static NOTIFICATIONS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
struct Identity {
    path: PathBuf,
    dev: u64,
    ino: u64,
    schema: i64,
}
struct Idle {
    connection: Connection,
    connection_id: u64,
    identity: Identity,
    read_only: bool,
}
static POOL: OnceLock<Mutex<Vec<Idle>>> = OnceLock::new();
static CHANGES: OnceLock<(Mutex<u64>, Condvar)> = OnceLock::new();

pub(crate) fn install_trace(connection: &Connection) -> Result<(), MachineError> {
    if crate::darwin::install_sqlite_statement_counters(connection, &STATEMENTS)
        != rusqlite::ffi::SQLITE_OK
    {
        return Err(failure("database statement counter cannot be registered"));
    }
    Ok(())
}
pub(crate) fn count_migration() {
    MIGRATIONS.fetch_add(1, Ordering::Relaxed);
}

fn failure(message: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable database observation failed",
        false,
        serde_json::json!({"invariant":message}),
    )
}
fn identity(path: &Path) -> Result<Identity, MachineError> {
    let parent = path
        .parent()
        .ok_or_else(|| failure("database parent is absent"))?;
    crate::workspace::verify_secure_directory(parent, crate::darwin::DarwinSystem.current_uid())?;
    crate::workspace::verify_secure_file(path, crate::darwin::DarwinSystem.current_uid())?;
    let metadata =
        fs::symlink_metadata(path).map_err(|_| failure("database identity is unavailable"))?;
    if metadata.nlink() != 1 {
        return Err(failure("database has an unexpected alias"));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|_| failure("database canonical identity is unavailable"))?;
    Ok(Identity {
        path: canonical,
        dev: metadata.dev(),
        ino: metadata.ino(),
        schema: 0,
    })
}

/// A lease removes its connection from the pool. No pool lock spans SQL,
/// dispatch, a transaction, a waiter, or an upstream call.
pub(crate) struct CheckedConnection {
    connection: Option<Connection>,
    connection_id: u64,
    identity: Identity,
    read_only: bool,
    _admission: Option<crate::ledger_generation::WorkspaceLease>,
}
impl CheckedConnection {
    pub(crate) fn open(path: &Path, read_only: bool) -> Result<Self, MachineError> {
        Self::open_with_admission(path, read_only, true)
    }
    pub(crate) fn open_maintenance(path: &Path) -> Result<Self, MachineError> {
        Self::open_with_admission(path, false, false)
    }
    fn open_with_admission(
        path: &Path,
        read_only: bool,
        admit: bool,
    ) -> Result<Self, MachineError> {
        let admission = if admit {
            database_admission(path)?
        } else {
            None
        };
        let mut selected = identity(path)?;
        let candidate = {
            let mut pool = POOL
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .map_err(|_| failure("database pool is poisoned"))?;
            let index = pool.iter().position(|idle| {
                idle.identity.path == selected.path && idle.read_only == read_only
            });
            index.map(|index| pool.swap_remove(index))
        };
        if let Some(idle) = candidate {
            if idle.identity.dev == selected.dev && idle.identity.ino == selected.ino {
                let schema: i64 = idle
                    .connection
                    .pragma_query_value(None, "schema_version", |row| row.get(0))
                    .map_err(|_| failure("database schema is unavailable"))?;
                // data_version observes WAL commits even when the main file is unchanged.
                let _: i64 = idle
                    .connection
                    .pragma_query_value(None, "data_version", |row| row.get(0))
                    .map_err(|_| failure("database durable version is unavailable"))?;
                if schema == idle.identity.schema {
                    selected.schema = schema;
                    REUSES.fetch_add(1, Ordering::Relaxed);
                    return Ok(Self {
                        connection: Some(idle.connection),
                        connection_id: idle.connection_id,
                        identity: selected,
                        read_only,
                        _admission: admission,
                    });
                }
            }
            INVALIDATIONS.fetch_add(1, Ordering::Relaxed);
        }
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        let connection = Connection::open_with_flags(
            &selected.path,
            flags | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| failure("database cannot be opened"))?;
        install_trace(&connection)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|_| failure("database timeout cannot be configured"))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|_| failure("database foreign-key guard cannot be configured"))?;
        if !read_only {
            connection
                .pragma_update(None, "synchronous", "FULL")
                .map_err(|_| failure("database durability guard cannot be configured"))?;
        }
        selected.schema = connection
            .pragma_query_value(None, "schema_version", |row| row.get(0))
            .map_err(|_| failure("database schema is unavailable"))?;
        let after = identity(&selected.path)?;
        if selected.dev != after.dev || selected.ino != after.ino {
            return Err(failure("database changed during open"));
        }
        OPENS.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            connection: Some(connection),
            connection_id: NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
            identity: selected,
            read_only,
            _admission: admission,
        })
    }
    pub(crate) fn observation_key(&self) -> (PathBuf, u64, u64, u64) {
        (
            self.identity.path.clone(),
            self.identity.dev,
            self.identity.ino,
            self.connection_id,
        )
    }
    pub(crate) fn observation_stamp(&self) -> Result<DatabaseObservationStamp, MachineError> {
        self.check_identity()?;
        let main = file_stamp(&self.identity.path)?;
        let wal = match fs::symlink_metadata(self.identity.path.with_file_name(format!(
                "{}-wal",
                self.identity
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| failure("database file name unavailable"))?
            ))) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(failure("database WAL identity unavailable")),
            Ok(_) => Some(file_stamp(&self.identity.path.with_file_name(format!(
                    "{}-wal",
                    self.identity
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or_else(|| failure("database file name unavailable"))?
                )))?),
        };
        Ok(DatabaseObservationStamp {
            main,
            wal,
            schema_version: self
                .pragma_query_value(None, "schema_version", |r| r.get(0))
                .map_err(|_| failure("database schema observation unavailable"))?,
            data_version: self
                .pragma_query_value(None, "data_version", |r| r.get(0))
                .map_err(|_| failure("database durable observation unavailable"))?,
            own_changes: self.total_changes(),
        })
    }
    pub(crate) fn check_identity(&self) -> Result<(), MachineError> {
        let now = identity(&self.identity.path)?;
        if now.dev != self.identity.dev || now.ino != self.identity.ino {
            return Err(failure("database replaced while leased"));
        }
        Ok(())
    }
}
impl Deref for CheckedConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.connection.as_ref().expect("live database lease")
    }
}
impl DerefMut for CheckedConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        self.connection.as_mut().expect("live database lease")
    }
}
impl Drop for CheckedConnection {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        if !connection.is_autocommit() || self.check_identity().is_err() {
            return;
        }
        let Ok(schema) =
            connection.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))
        else {
            return;
        };
        self.identity.schema = schema;
        if let Ok(mut pool) = POOL.get_or_init(|| Mutex::new(Vec::new())).lock() {
            if pool.len() >= 32 {
                pool.remove(0);
            }
            pool.push(Idle {
                connection,
                connection_id: self.connection_id,
                identity: self.identity.clone(),
                read_only: self.read_only,
            });
        }
    }
}

pub(crate) fn epoch() -> Result<u64, MachineError> {
    let (lock, _) = CHANGES.get_or_init(|| (Mutex::new(0), Condvar::new()));
    Ok(*lock
        .lock()
        .map_err(|_| failure("database notification epoch is poisoned"))?)
}
pub(crate) fn wait(observed: u64, duration: Duration) -> Result<(), MachineError> {
    let (lock, changed) = CHANGES.get_or_init(|| (Mutex::new(0), Condvar::new()));
    let guard = lock
        .lock()
        .map_err(|_| failure("database notification epoch is poisoned"))?;
    WAITS.fetch_add(1, Ordering::Relaxed);
    drop(
        changed
            .wait_timeout_while(guard, duration, |epoch| *epoch == observed)
            .map_err(|_| failure("database notification wait is poisoned"))?,
    );
    Ok(())
}
pub(crate) fn commit(transaction: Transaction<'_>) -> rusqlite::Result<()> {
    transaction.commit()?;
    // Notify only after the durable FULL-synchronous commit is visible.
    let (lock, changed) = CHANGES.get_or_init(|| (Mutex::new(0), Condvar::new()));
    if let Ok(mut epoch) = lock.lock() {
        *epoch = epoch.wrapping_add(1);
        changed.notify_all();
    }
    NOTIFICATIONS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

#[must_use]
pub fn counters() -> BTreeMap<&'static str, u64> {
    let (queries, ddl) = STATEMENTS.snapshot();
    BTreeMap::from([
        ("database_queries", queries),
        ("database_ddl", ddl),
        ("database_migrations", MIGRATIONS.load(Ordering::Relaxed)),
        ("database_physical_opens", OPENS.load(Ordering::Relaxed)),
        ("database_connection_reuses", REUSES.load(Ordering::Relaxed)),
        (
            "database_connection_invalidations",
            INVALIDATIONS.load(Ordering::Relaxed),
        ),
        ("database_waits", WAITS.load(Ordering::Relaxed)),
        (
            "database_commit_notifications",
            NOTIFICATIONS.load(Ordering::Relaxed),
        ),
    ])
}

pub(crate) fn initialize_content(connection: &mut Connection) -> Result<(), MachineError> {
    use rusqlite::OptionalExtension as _;
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key='result_content_schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure("result content version is unavailable"))?;
    if version.as_deref() == Some("3") {
        return Ok(());
    }
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| failure("result content migration cannot acquire ownership"))?;
    let locked: Option<String> = transaction
        .query_row(
            "SELECT value FROM metadata WHERE key='result_content_schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure("result content version is unavailable"))?;
    if locked.is_none() {
        count_migration();
        transaction
            .execute_batch(
                "CREATE TABLE result_content(sha256 TEXT PRIMARY KEY,bytes BLOB NOT NULL) STRICT;
             CREATE TABLE result_content_refs(
               owner_kind TEXT NOT NULL,owner_id TEXT NOT NULL,representation TEXT NOT NULL,
               sha256 TEXT NOT NULL,byte_length INTEGER NOT NULL,retired_operation TEXT,
               PRIMARY KEY(owner_kind,owner_id,representation)
             ) STRICT;
             CREATE INDEX result_content_live_refs ON result_content_refs(sha256,retired_operation);
             INSERT INTO metadata(key,value) VALUES('result_content_schema_version','1');",
            )
            .map_err(|_| failure("result content migration failed"))?;
    } else if !matches!(locked.as_deref(), Some("1" | "2" | "3")) {
        return Err(failure("result content schema is unsupported"));
    }
    if !matches!(locked.as_deref(), Some("2" | "3")) {
        transaction.execute_batch(
            "CREATE TABLE brokered_chunk_bindings(
               reference_id TEXT UNIQUE NOT NULL,session_id TEXT NOT NULL,source_tool_call_id TEXT NOT NULL,
               task_id TEXT NOT NULL,offset INTEGER NOT NULL,byte_length INTEGER NOT NULL,response_sha256 TEXT NOT NULL,
               PRIMARY KEY(session_id,source_tool_call_id)
             ) STRICT;
             CREATE INDEX brokered_chunk_task ON brokered_chunk_bindings(task_id,reference_id);
             UPDATE metadata SET value='2' WHERE key='result_content_schema_version';"
        ).map_err(|_|failure("chunk receipt migration failed"))?;
    }
    if locked.as_deref() != Some("3") {
        transaction.execute_batch(
            "CREATE TABLE brokered_result_capacity_failures(
               task_id TEXT PRIMARY KEY,target_turn_id TEXT NOT NULL,byte_length INTEGER NOT NULL,
               result_sha256 TEXT NOT NULL,result_json_sha256 TEXT NOT NULL,created_at_ms INTEGER NOT NULL,
               FOREIGN KEY(task_id) REFERENCES brokered_tasks(task_id)
             ) STRICT;
             UPDATE metadata SET value='3' WHERE key='result_content_schema_version';"
        ).map_err(|_|failure("result capacity proof migration failed"))?;
    }
    commit(transaction).map_err(|_| failure("result content migration commit failed"))
}

pub(crate) fn put_content(
    connection: &Connection,
    kind: &str,
    owner: &str,
    representation: &str,
    bytes: &[u8],
) -> Result<String, MachineError> {
    let digest = crate::jcs::sha256_hex(bytes);
    connection
        .execute(
            "INSERT OR IGNORE INTO result_content(sha256,bytes) VALUES(?1,?2)",
            rusqlite::params![digest, bytes],
        )
        .map_err(|_| failure("result content publication failed"))?;
    let retained: Vec<u8> = connection
        .query_row(
            "SELECT bytes FROM result_content WHERE sha256=?1",
            [&digest],
            |row| row.get(0),
        )
        .map_err(|_| failure("result content publication cannot be verified"))?;
    if retained != bytes {
        return Err(failure("immutable result digest collision or corruption"));
    }
    connection.execute(
        "INSERT OR IGNORE INTO result_content_refs(owner_kind,owner_id,representation,sha256,byte_length)
         VALUES(?1,?2,?3,?4,?5)", rusqlite::params![kind, owner, representation, digest, bytes.len() as u64],
    ).map_err(|_| failure("result content reference publication failed"))?;
    let actual = content_reference(connection, kind, owner, representation)?
        .ok_or_else(|| failure("result content reference is absent"))?;
    if actual.sha256 != digest
        || actual.byte_length != bytes.len() as u64
        || actual.retired_operation.is_some()
    {
        return Err(failure("immutable result reference changed"));
    }
    Ok(digest)
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContentReference {
    pub owner_kind: String,
    pub owner_id: String,
    pub representation: String,
    pub sha256: String,
    pub byte_length: u64,
    pub retired_operation: Option<uuid::Uuid>,
}
pub(crate) fn content_reference(
    connection: &Connection,
    kind: &str,
    owner: &str,
    representation: &str,
) -> Result<Option<ContentReference>, MachineError> {
    use rusqlite::OptionalExtension as _;
    let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='result_content_refs' AND type='table')", [], |row| row.get(0))
        .map_err(|_| failure("result content namespace is unavailable"))?;
    if !exists {
        return Ok(None);
    }
    let row: Option<(String,u64,Option<String>)> = connection.query_row(
        "SELECT sha256,byte_length,retired_operation FROM result_content_refs WHERE owner_kind=?1 AND owner_id=?2 AND representation=?3",
        rusqlite::params![kind, owner, representation], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).optional().map_err(|_| failure("result content reference is unavailable"))?;
    row.map(|(sha256, byte_length, retired)| {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(failure("result content digest is invalid"));
        }
        let retired_operation = retired
            .map(|id| {
                uuid::Uuid::parse_str(&id)
                    .ok()
                    .filter(|id| id.get_version_num() == 7)
                    .ok_or_else(|| failure("result tombstone identity is invalid"))
            })
            .transpose()?;
        Ok(ContentReference {
            owner_kind: kind.to_owned(),
            owner_id: owner.to_owned(),
            representation: representation.to_owned(),
            sha256,
            byte_length,
            retired_operation,
        })
    })
    .transpose()
}
pub(crate) fn read_content(
    connection: &Connection,
    kind: &str,
    owner: &str,
    representation: &str,
) -> Result<Option<Vec<u8>>, MachineError> {
    let Some(reference) = content_reference(connection, kind, owner, representation)? else {
        return Ok(None);
    };
    read_referenced_content(connection, &reference).map(Some)
}

pub(crate) fn read_bound_content(
    connection: &Connection,
    kind: &str,
    owner: &str,
    representation: &str,
    expected_sha256: &str,
) -> Result<Option<Vec<u8>>, MachineError> {
    let Some(reference) = content_reference(connection, kind, owner, representation)? else {
        return Ok(None);
    };
    if reference.sha256 != expected_sha256 {
        return Err(failure(
            "content reference differs from its original result digest",
        ));
    }
    read_referenced_content(connection, &reference).map(Some)
}

/// Read the immutable value selected by an already checked reference capture.
pub(crate) fn read_referenced_content(
    connection: &Connection,
    reference: &ContentReference,
) -> Result<Vec<u8>, MachineError> {
    if let Some(operation) = reference.retired_operation {
        return Err(MachineError::new(
            "RESULT_CONTENT_UNAVAILABLE",
            "result content was explicitly retired",
            false,
            serde_json::json!({"operation_ref":operation,"required_action":"inspect_retained_identity"}),
        ));
    }
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT bytes FROM result_content WHERE sha256=?1",
            [&reference.sha256],
            |row| row.get(0),
        )
        .map_err(|_| failure("published result content is absent"))?;
    if bytes.len() as u64 != reference.byte_length
        || crate::jcs::sha256_hex(&bytes) != reference.sha256
    {
        return Err(failure(
            "published result content failed integrity validation",
        ));
    }
    Ok(bytes)
}

#[derive(serde::Serialize, serde::Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ContentPointer {
    schema: String,
    artifact_id: uuid::Uuid,
    sha256: String,
    byte_length: u64,
}
fn pointer(artifact: uuid::Uuid, digest: &str, length: u64) -> ContentPointer {
    ContentPointer {
        schema: "dolgorae.native-result-content-reference/v1".to_owned(),
        artifact_id: artifact,
        sha256: digest.to_owned(),
        byte_length: length,
    }
}
pub(crate) fn write_shared_artifact(
    root: &Path,
    run: uuid::Uuid,
    artifact: uuid::Uuid,
    digest: &str,
    bytes: &[u8],
) -> Result<(), MachineError> {
    if crate::jcs::sha256_hex(bytes) != digest {
        return Err(failure("shared content digest disagrees"));
    }
    let uid = crate::darwin::DarwinSystem.current_uid();
    let content_root = root.join("result-content");
    match crate::workspace::create_directory(&content_root, 0o700) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(failure("shared content directory cannot be created")),
    }
    crate::workspace::verify_secure_directory(&content_root, uid)?;
    let body = content_root.join(format!("{digest}.bin"));
    match crate::workspace::atomic_create(
        &crate::workspace::SystemWorkspacePlatform,
        &body,
        bytes,
        0o600,
    ) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(failure("shared content cannot be published")),
    }
    let artifact_root = crate::run::run_root(root, run).join("artifacts");
    match crate::workspace::create_directory(&artifact_root, 0o700) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(failure("artifact reference directory cannot be created")),
    }
    crate::workspace::verify_secure_directory(&artifact_root, uid)?;
    let reference = serde_json::to_vec(&pointer(artifact, digest, bytes.len() as u64))
        .map_err(|_| failure("artifact reference cannot be encoded"))?;
    match crate::workspace::atomic_create(
        &crate::workspace::SystemWorkspacePlatform,
        &artifact_root.join(format!("{artifact}.bin")),
        &reference,
        0o600,
    ) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(failure("artifact reference cannot be published")),
    }
    let mut actual = open_shared_artifact(root, run, artifact, digest, bytes.len() as u64)?;
    let mut verified = Vec::new();
    std::io::Read::read_to_end(&mut actual, &mut verified)
        .map_err(|_| failure("shared content cannot be read"))?;
    if verified != bytes {
        return Err(failure("shared content publication changed"));
    }
    Ok(())
}
pub(crate) fn open_shared_artifact(
    root: &Path,
    run: uuid::Uuid,
    artifact: uuid::Uuid,
    digest: &str,
    length: u64,
) -> Result<std::fs::File, MachineError> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let uid = crate::darwin::DarwinSystem.current_uid();
    let mut directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(root)
        .map_err(|_| failure("shared content workspace is unavailable"))?;
    let secure = |file: &std::fs::File| -> Result<(), MachineError> {
        let metadata = file
            .metadata()
            .map_err(|_| failure("content directory identity is unavailable"))?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
            return Err(failure("content directory is untrusted"));
        }
        Ok(())
    };
    secure(&directory)?;
    let content = crate::workspace::open_relative_nofollow(
        &directory,
        std::ffi::OsStr::new("result-content"),
        true,
    )
    .map_err(|_| failure("shared content directory is unavailable"))?;
    secure(&content)?;
    for part in ["runs".to_owned(), run.to_string(), "artifacts".to_owned()] {
        directory =
            crate::workspace::open_relative_nofollow(&directory, std::ffi::OsStr::new(&part), true)
                .map_err(|_| failure("artifact reference directory is unavailable"))?;
        secure(&directory)?;
    }
    let mut reference = crate::workspace::open_relative_nofollow(
        &directory,
        std::ffi::OsStr::new(&format!("{artifact}.bin")),
        false,
    )
    .map_err(|_| failure("artifact reference is unavailable"))?;
    let metadata = reference
        .metadata()
        .map_err(|_| failure("artifact reference identity unavailable"))?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > 1024
    {
        return Err(failure("artifact reference is untrusted"));
    }
    let before = (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    );
    let mut bytes = Vec::new();
    reference
        .by_ref()
        .take(1025)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("artifact reference cannot be read"))?;
    let after = reference
        .metadata()
        .map_err(|_| failure("artifact reference identity unavailable"))?;
    let actual: ContentPointer =
        serde_json::from_slice(&bytes).map_err(|_| failure("artifact reference is malformed"))?;
    if actual != pointer(artifact, digest, length)
        || before
            != (
                after.dev(),
                after.ino(),
                after.len(),
                after.ctime(),
                after.ctime_nsec(),
            )
    {
        return Err(failure("artifact reference changed"));
    }
    let file = crate::workspace::open_relative_nofollow(
        &content,
        std::ffi::OsStr::new(&format!("{digest}.bin")),
        false,
    )
    .map_err(|_| failure("published shared content is unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("shared content identity is unavailable"))?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() != length
    {
        return Err(failure("shared content is untrusted"));
    }
    Ok(file)
}

pub(crate) fn database_admission(
    path: &Path,
) -> Result<Option<crate::ledger_generation::WorkspaceLease>, MachineError> {
    let Some(parent) = path.parent().filter(|parent| {
        parent
            .file_name()
            .is_some_and(|name| name == "orchestration")
    }) else {
        return Ok(None);
    };
    let Some(root) = parent.parent() else {
        return Err(failure("database workspace is absent"));
    };
    match fs::symlink_metadata(root.join("workspace.json")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(failure("database workspace authority unavailable")),
        Ok(_) => crate::ledger_generation::WorkspaceLease::acquire(root, false)
            .map(Some)
            .map_err(|error| match error {
                crate::ledger::LedgerError::SecurityPolicy(error) => error,
                crate::ledger::LedgerError::WriterBusy(_) => MachineError::new(
                    "CAPABILITY_UNSUPPORTED",
                    "workspace maintenance excludes database access; inspect and resume the original maintenance operation",
                    false,
                    serde_json::json!({"profile":null,"capability_name":"dolgorae.workspace-maintenance/v014","support":"recognized_unsupported"}),
                ),
                _ => failure("workspace maintenance admission failed"),
            }),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DatabaseObservationStamp {
    main: DatabaseFileStamp,
    wal: Option<DatabaseFileStamp>,
    schema_version: i64,
    data_version: i64,
    own_changes: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct DatabaseFileStamp {
    device: u64,
    inode: u64,
    length: u64,
    uid: u32,
    mode: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn file_stamp(path: &Path) -> Result<DatabaseFileStamp, MachineError> {
    crate::workspace::verify_secure_file(path, crate::darwin::DarwinSystem.current_uid())?;
    let metadata =
        fs::symlink_metadata(path).map_err(|_| failure("database source identity unavailable"))?;
    if metadata.nlink() != 1 {
        return Err(failure("database source alias is untrusted"));
    }
    Ok(DatabaseFileStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        uid: metadata.uid(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

/// Fixed, parameter-bound query-plan diagnostics. Statement and bind values
/// are not included; SQLite's planner details contain only table/index names.
pub fn query_plans(path: &Path) -> Result<BTreeMap<&'static str, Vec<String>>, MachineError> {
    let connection = CheckedConnection::open(path, true)?;
    let queries = [
        (
            "prepared_result_recovery",
            "EXPLAIN QUERY PLAN SELECT task_id FROM brokered_result_publications WHERE state=?1",
        ),
        (
            "session_event_tail",
            "EXPLAIN QUERY PLAN SELECT event_hash FROM orchestration_events WHERE session_id=?1 AND sequence>0 ORDER BY sequence LIMIT 500",
        ),
        (
            "session_active_tasks",
            "EXPLAIN QUERY PLAN SELECT task_id FROM brokered_tasks WHERE session_id=?1 AND state='running' LIMIT 16",
        ),
        (
            "external_task_lookup",
            "EXPLAIN QUERY PLAN SELECT task_id FROM tasks WHERE task_id=?1",
        ),
        (
            "chunk_call_binding",
            "EXPLAIN QUERY PLAN SELECT reference_id FROM brokered_chunk_bindings WHERE session_id=?1 AND source_tool_call_id='diagnostic-placeholder'",
        ),
        (
            "chunk_task_binding",
            "EXPLAIN QUERY PLAN SELECT reference_id FROM brokered_chunk_bindings WHERE task_id=?1 LIMIT 4096",
        ),
        (
            "content_protected_references",
            "EXPLAIN QUERY PLAN SELECT owner_id FROM result_content_refs WHERE sha256=?1 AND retired_operation IS NULL LIMIT 4096",
        ),
    ];
    let mut plans = BTreeMap::new();
    for (name, query) in queries {
        let mut statement = connection
            .prepare(query)
            .map_err(|_| failure("observation query plan unavailable"))?;
        let rows = statement
            .query_map(["diagnostic-placeholder"], |row| row.get::<_, String>(3))
            .map_err(|_| failure("observation query plan unavailable"))?;
        plans.insert(
            name,
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|_| failure("observation query plan malformed"))?,
        );
    }
    Ok(plans)
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChunkBinding {
    pub reference_id: uuid::Uuid,
    pub session_id: uuid::Uuid,
    pub source_tool_call_id: String,
    pub task_id: uuid::Uuid,
    pub offset: u64,
    pub byte_length: u64,
    pub response_sha256: String,
}
pub(crate) fn chunk_bindings(connection: &Connection) -> Result<Vec<ChunkBinding>, MachineError> {
    let mut statement=connection.prepare("SELECT reference_id,session_id,source_tool_call_id,task_id,offset,byte_length,response_sha256 FROM brokered_chunk_bindings ORDER BY reference_id LIMIT 4097")
        .map_err(|_|failure("chunk receipt bindings unavailable"))?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, u64>(4)?,
                r.get::<_, u64>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(|_| failure("chunk receipt bindings unavailable"))?;
    let mut bindings = Vec::new();
    for row in rows {
        let (reference, session, call, task, offset, length, digest) =
            row.map_err(|_| failure("chunk receipt binding malformed"))?;
        let uuid = |id: &str| {
            uuid::Uuid::parse_str(id)
                .ok()
                .filter(|id| id.get_version_num() == 7)
                .ok_or_else(|| failure("chunk receipt UUID binding malformed"))
        };
        if call.is_empty()
            || call.len() > 4096
            || length > 65536
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(failure("chunk receipt binding malformed"));
        }
        bindings.push(ChunkBinding {
            reference_id: uuid(&reference)?,
            session_id: uuid(&session)?,
            source_tool_call_id: call,
            task_id: uuid(&task)?,
            offset,
            byte_length: length,
            response_sha256: digest,
        });
    }
    if bindings.len() > 4096 {
        return Err(MachineError::new(
            "STORAGE_CAPACITY_EXCEEDED",
            "chunk receipt maintenance snapshot exceeds its bound",
            false,
            serde_json::json!({}),
        ));
    }
    Ok(bindings)
}
pub(crate) fn chunk_binding(
    connection: &Connection,
    session: uuid::Uuid,
    call: &str,
) -> Result<Option<ChunkBinding>, MachineError> {
    use rusqlite::OptionalExtension as _;
    let exists:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='brokered_chunk_bindings')",[],|r|r.get(0)).map_err(|_|failure("chunk receipt namespace unavailable"))?;
    if !exists {
        return Ok(None);
    }
    let row:Option<(String,String,u64,u64,String)>=connection.query_row("SELECT reference_id,task_id,offset,byte_length,response_sha256 FROM brokered_chunk_bindings WHERE session_id=?1 AND source_tool_call_id=?2",
        rusqlite::params![session.to_string(),call],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|_|failure("chunk receipt binding unavailable"))?;
    row.map(|(reference, task, offset, length, response_sha256)| {
        let uuid = |id: &str| {
            uuid::Uuid::parse_str(id)
                .ok()
                .filter(|id| id.get_version_num() == 7)
                .ok_or_else(|| failure("chunk receipt UUID binding malformed"))
        };
        if length > 65536
            || response_sha256.len() != 64
            || !response_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(failure("chunk receipt binding malformed"));
        }
        Ok(ChunkBinding {
            reference_id: uuid(&reference)?,
            session_id: session,
            source_tool_call_id: call.to_owned(),
            task_id: uuid(&task)?,
            offset,
            byte_length: length,
            response_sha256,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dolgorae-storage-observation-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("database.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE values_table(value INTEGER);")
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(connection);
        path
    }
    #[test]
    fn sqlite_callback_counts_executed_queries_and_ddl_without_retaining_canary_text() {
        let connection = Connection::open_in_memory().unwrap();
        install_trace(&connection).unwrap();
        let before_queries = STATEMENTS.snapshot().0;
        let before_ddl = STATEMENTS.snapshot().1;
        connection
            .execute_batch("CREATE TABLE trace_canary(body TEXT); DROP TABLE trace_canary;")
            .unwrap();
        assert!(STATEMENTS.snapshot().0 >= before_queries + 2);
        assert!(STATEMENTS.snapshot().1 >= before_ddl + 2);
        let before_query = STATEMENTS.snapshot().0;
        let canary = "private-binding-canary-should-never-be-retained";
        let returned: String = connection
            .query_row(
                "SELECT ?1 || ' private-sql-canary-should-never-be-retained'",
                [canary],
                |row| row.get(0),
            )
            .unwrap();
        assert!(returned.starts_with(canary));
        assert!(STATEMENTS.snapshot().0 > before_query);
        let snapshot = counters();
        assert!(snapshot.keys().all(|name| name.starts_with("database_")));
        let exported = serde_json::to_string(&snapshot).unwrap();
        assert!(!exported.contains(canary));
        assert!(!exported.contains("private-sql-canary"));
        assert!(!exported.contains("SELECT"));
        assert!(!exported.contains("trace_canary"));
    }
    #[test]
    fn tombstones_reject_non_v7_identity_and_preserve_original_operation() {
        let path = fixture();
        let mut connection = CheckedConnection::open(&path, false).unwrap();
        connection
            .execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);")
            .unwrap();
        initialize_content(&mut connection).unwrap();
        let owner = uuid::Uuid::now_v7().to_string();
        let body = b"retained result";
        let digest = put_content(&connection, "external_artifact", &owner, "raw", body).unwrap();
        connection
            .execute(
                "UPDATE result_content_refs SET retired_operation=?1 WHERE owner_id=?2",
                rusqlite::params!["123e4567-e89b-42d3-a456-426614174000", owner],
            )
            .unwrap();
        assert_eq!(
            read_content(&connection, "external_artifact", &owner, "raw")
                .unwrap_err()
                .code,
            "INTERNAL_ERROR"
        );
        let operation = uuid::Uuid::now_v7();
        connection
            .execute(
                "UPDATE result_content_refs SET retired_operation=?1 WHERE owner_id=?2",
                rusqlite::params![operation.to_string(), owner],
            )
            .unwrap();
        let unavailable =
            read_content(&connection, "external_artifact", &owner, "raw").unwrap_err();
        assert_eq!(unavailable.code, "RESULT_CONTENT_UNAVAILABLE");
        assert_eq!(unavailable.details["operation_ref"], operation.to_string());
        assert_eq!(
            connection
                .query_row(
                    "SELECT bytes FROM result_content WHERE sha256=?1",
                    [&digest],
                    |row| row.get::<_, Vec<u8>>(0)
                )
                .unwrap(),
            body
        );
        assert_eq!(
            content_reference(&connection, "external_artifact", &owner, "raw")
                .unwrap()
                .unwrap()
                .retired_operation,
            Some(operation)
        );
        drop(connection);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn replaced_database_is_not_served_by_cached_connection() {
        let path = fixture();
        let first = CheckedConnection::open(&path, true).unwrap();
        let inode = first.identity.ino;
        drop(first);
        fs::rename(&path, path.with_extension("old")).unwrap();
        let replacement = Connection::open(&path).unwrap();
        replacement
            .execute_batch("CREATE TABLE replacement(value INTEGER);")
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(replacement);
        let second = CheckedConnection::open(&path, true).unwrap();
        assert_ne!(second.identity.ino, inode);
        assert!(second.prepare("SELECT * FROM replacement").is_ok());
        drop(second);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn pooled_reader_observes_wal_commit_without_main_file_change() {
        let path = fixture();
        let reader = CheckedConnection::open(&path, true).unwrap();
        let before: i64 = reader
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .unwrap();
        let writer = Connection::open(&path).unwrap();
        writer
            .execute("INSERT INTO values_table VALUES(7)", [])
            .unwrap();
        let after: i64 = reader
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .unwrap();
        assert_ne!(before, after);
        assert_eq!(
            reader
                .query_row("SELECT value FROM values_table", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            7
        );
        drop(reader);
        drop(writer);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn commit_between_predicate_and_wait_cannot_be_lost() {
        let path = fixture();
        let mut writer = CheckedConnection::open(&path, false).unwrap();
        let observed = epoch().unwrap();
        let transaction = writer.transaction().unwrap();
        transaction
            .execute("INSERT INTO values_table VALUES(9)", [])
            .unwrap();
        commit(transaction).unwrap();
        let started = std::time::Instant::now();
        wait(observed, Duration::from_secs(1)).unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        drop(writer);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
