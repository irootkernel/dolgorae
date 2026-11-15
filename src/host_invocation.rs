//! Protected native identities and K-only product linkage.
//!
//! This is storage and authentication, not an MCP dispatcher. A delivery decision
//! never constitutes product acceptance; only an authorized product lookup can
//! reconcile an ambiguous K delivery. R is always a new observation, and S can
//! only reconcile once its durable delivery marker has been written.

use crate::delegation_identity::{LookupNamespace, ProtectedInvocation, TrustedInvocationContext};
use crate::machine::MachineError;
use crate::trusted_host::{
    HostLock, HostRoute, OwnerKind, VerifiedOwner, canonical, conflict, digest, opaque, refusal,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use uuid::Uuid;
use zeroize::Zeroize as _;

const DATABASE: &str = "invocations.sqlite";
const STAGING: &str = "invocations.pending";
const MAX_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_REQUEST: usize = 16 * 1024 * 1024;
const DDL: &str = "
CREATE TABLE metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 schema_version INTEGER NOT NULL CHECK(schema_version=1), owner_digest TEXT NOT NULL,
 birth_id TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation>0));
CREATE TABLE native_calls(event_digest TEXT PRIMARY KEY, session_ref TEXT NOT NULL, tool_use_ref TEXT NOT NULL, record_json TEXT NOT NULL,
 attempted INTEGER NOT NULL DEFAULT 0 CHECK(attempted IN (0,1)));
CREATE UNIQUE INDEX native_delivery ON native_calls(session_ref,tool_use_ref);
";

/// The selected native hook supplies these fields before tool delivery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEvent {
    pub session: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub turn: Option<String>,
    pub tool_use: String,
    pub server: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub server_source: Option<String>,
    pub tool: String,
}

impl NativeEvent {
    fn validate(&self) -> Result<(), MachineError> {
        if !opaque(&self.session, 512)
            || !opaque(&self.tool_use, 512)
            || self.turn.as_ref().is_some_and(|turn| !opaque(turn, 512))
            || self.server != "dolgorae"
            || !opaque(&self.tool, 128)
        {
            return Err(refusal());
        }
        Ok(())
    }
}

/// Private carrier bytes. Never serialize, Debug-print or return in a tool result.
pub struct Capability([u8; 32]);
impl Capability {
    pub fn from_private_carrier(bytes: &[u8]) -> Result<Self, MachineError> {
        Ok(Self(bytes.try_into().map_err(|_| refusal())?))
    }
    pub fn private_carrier_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    fn random() -> Result<Self, MachineError> {
        let mut value = Self([0; 32]);
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut value.0))
            .map_err(|_| refusal())?;
        Ok(value)
    }
    fn matches(&self, other: &[u8]) -> bool {
        other.len() == 32
            && self
                .0
                .iter()
                .zip(other)
                .fold(0, |diff, (a, b)| diff | (a ^ b))
                == 0
    }
}
impl Drop for Capability {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MethodClass {
    Keyed,
    Read,
    State,
}

pub fn method_class(operation: &str) -> Result<MethodClass, MachineError> {
    match operation {
        "install_provisioning"
        | "open_external_engagement"
        | "hire_external_specialist"
        | "assign_external_specialist_task"
        | "spawn_specialist"
        | "assign_specialist_task"
        | "assign_group"
        | "assign_synthesis"
        | "merge_group_results" => Ok(MethodClass::Keyed),
        "validate_provisioning"
        | "get_external_engagement"
        | "await_external_specialist_task"
        | "collect_external_specialist_task"
        | "get_external_delegation_identity"
        | "lookup_operation"
        | "await_specialist_task"
        | "collect_specialist_task"
        | "get_managed_delegation_identity"
        | "await_group"
        | "collect_group" => Ok(MethodClass::Read),
        "close_external_engagement"
        | "cancel_external_specialist_task"
        | "release_external_specialist"
        | "cancel_specialist_task"
        | "release_specialist" => Ok(MethodClass::State),
        _ => Err(refusal()),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    event: NativeEvent,
    context: TrustedInvocationContext,
    request: Value,
    capability: Vec<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    keyed: Option<ProtectedInvocation>,
}
impl Drop for Record {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

/// A selected and authenticated native binding; fields cannot be model-built.
pub struct BoundInvocation {
    owner_digest: String,
    owner_path: std::path::PathBuf,
    event: NativeEvent,
    event_digest: String,
    context: TrustedInvocationContext,
    request: Value,
    namespace: Option<LookupNamespace>,
}
impl BoundInvocation {
    pub(crate) fn require_owner(&self, owner: &VerifiedOwner) -> Result<(), MachineError> {
        owner.revalidate()?;
        if self.owner_digest != owner.configuration_digest
            || self.owner_path != owner.path
            || self.context.host_route != owner.configuration.route.identifier()
            || self.context.tool_binding != self.event.tool
            || !owner.configuration.tools.contains(&self.event.tool)
        {
            return Err(refusal());
        }
        Ok(())
    }
    pub(crate) fn native_event(&self) -> &NativeEvent {
        &self.event
    }
    pub fn context(&self) -> &TrustedInvocationContext {
        &self.context
    }
    pub(crate) fn namespace(&self) -> Result<&LookupNamespace, MachineError> {
        self.namespace.as_ref().ok_or_else(refusal)
    }
    pub fn request(&self) -> &Value {
        &self.request
    }
    pub fn trusted_keyed(&self) -> Result<crate::caller_scope::TrustedInvocation, MachineError> {
        crate::caller_scope::TrustedInvocation::from_port(self)
    }
}
impl crate::caller_scope::sealed::Sealed for BoundInvocation {}
impl crate::caller_scope::TrustedInvocationPort for BoundInvocation {
    fn invocation(&self) -> Result<(TrustedInvocationContext, LookupNamespace), MachineError> {
        Ok((
            self.context.clone(),
            self.namespace.clone().ok_or_else(refusal)?,
        ))
    }
}

/// The caller must reconcile through its authoritative getter/lookup on repeats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    Dispatch,
    ObserveFresh,
    Reconcile,
}

/// A complete setup and current owner carrier are prerequisites to every access.
pub struct HostStore {
    owner: VerifiedOwner,
    connection: Connection,
    database: File,
    generation: u64,
    // Held for the store's lifetime, including marker publication and dispatch handoff.
    _lock: HostLock,
}

pub(crate) fn persistence_entry_exists(path: &Path) -> Result<bool, MachineError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(refusal()),
    }
}

pub(crate) fn require_no_sidecars(owner: &VerifiedOwner, name: &str) -> Result<(), MachineError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        // A remnant of either database name is recovery evidence even when
        // the database itself was never published. Never let SQLite open it.
        if persistence_entry_exists(&owner.root.path.join(format!("{name}{suffix}")))? {
            return Err(refusal());
        }
    }
    Ok(())
}

pub(crate) fn database_connection(
    owner: &VerifiedOwner,
    name: &str,
) -> Result<(Connection, File), MachineError> {
    let path = owner.root.path.join(name);
    let file = owner.root.open_file(&path)?;
    require_no_sidecars(owner, name)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| refusal())?;
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|_| refusal())?;
    if mode != "delete" {
        return Err(refusal());
    }
    connection
        .execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .map_err(|_| refusal())?;
    verify_database(owner, &file, name)?;
    Ok((connection, file))
}

pub(crate) fn verify_database(
    owner: &VerifiedOwner,
    file: &File,
    name: &str,
) -> Result<(), MachineError> {
    let current = owner.root.open_file(&owner.root.path.join(name))?;
    let before = file.metadata().map_err(|_| refusal())?;
    let after = current.metadata().map_err(|_| refusal())?;
    if before.dev() != after.dev() || before.ino() != after.ino() {
        return Err(refusal());
    }
    Ok(())
}

pub(crate) fn initialize(owner: &VerifiedOwner) -> Result<(), MachineError> {
    owner.revalidate()?;
    let birth = crate::engagement::require_delegation_namespace(&owner.state_root)?;
    let path = owner.root.path.join(DATABASE);
    require_no_sidecars(owner, DATABASE)?;
    require_no_sidecars(owner, STAGING)?;
    if persistence_entry_exists(&owner.root.path.join(STAGING))? {
        return Err(refusal());
    }
    if persistence_entry_exists(&path)? {
        let (connection, _) = database_connection(owner, DATABASE)?;
        return metadata(owner, &connection, birth).map(|_| ());
    }
    // Exclusive staging is a durable interruption marker; never remove/reset it.
    let staging = owner.root.create_file(STAGING)?;
    let (mut connection, _) = database_connection(owner, STAGING)?;
    let random = Capability::random()?;
    let mut generation_bytes = [0; 8];
    generation_bytes.copy_from_slice(&random.0[..8]);
    let generation = u64::from_le_bytes(generation_bytes) % MAX_INTEGER + 1;
    let tx = connection.transaction().map_err(|_| refusal())?;
    tx.execute_batch(DDL).map_err(|_| refusal())?;
    tx.execute(
        "INSERT INTO metadata VALUES(1,1,?1,?2,?3)",
        params![owner.configuration_digest, birth.to_string(), generation],
    )
    .map_err(|_| refusal())?;
    tx.commit().map_err(|_| refusal())?;
    connection.close().map_err(|_| refusal())?;
    staging.sync_all().map_err(|_| refusal())?;
    owner.root.verify()?;
    crate::darwin::DarwinSystem
        .renameat_exclusive(
            &owner.root.file,
            std::ffi::OsStr::new(STAGING),
            std::ffi::OsStr::new(DATABASE),
        )
        .map_err(|_| refusal())?;
    owner.root.file.sync_all().map_err(|_| refusal())?;
    Ok(())
}

fn metadata(
    owner: &VerifiedOwner,
    connection: &Connection,
    birth: Uuid,
) -> Result<u64, MachineError> {
    let (version, owner_digest, recorded_birth, generation): (u32, String, String, u64) = connection
        .query_row("SELECT schema_version,owner_digest,birth_id,generation FROM metadata WHERE singleton=1",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).map_err(|_| refusal())?;
    let count: u64 = connection
        .query_row("SELECT COUNT(*) FROM metadata", [], |row| row.get(0))
        .map_err(|_| refusal())?;
    if version != 1
        || owner_digest != owner.configuration_digest
        || recorded_birth != birth.to_string()
        || !(1..=MAX_INTEGER).contains(&generation)
        || count != 1
    {
        return Err(refusal());
    }
    connection.prepare("SELECT event_digest,session_ref,tool_use_ref,record_json,attempted FROM native_calls LIMIT 0").map_err(|_| refusal())?;
    Ok(generation)
}

/// Read the initialized adapter generation without retaining the host lock.
pub(crate) fn preparation_generation(owner: &VerifiedOwner) -> Result<u64, MachineError> {
    let _lock = owner.root.lock()?;
    owner.revalidate()?;
    require_no_sidecars(owner, STAGING)?;
    if persistence_entry_exists(&owner.root.path.join(STAGING))? {
        return Err(refusal());
    }
    let birth = crate::engagement::require_delegation_namespace(&owner.state_root)?;
    let (connection, _database) = database_connection(owner, DATABASE)?;
    metadata(owner, &connection, birth)
}

impl HostStore {
    pub(crate) fn open(owner: VerifiedOwner) -> Result<Self, MachineError> {
        let lock = owner.root.lock()?;
        owner.revalidate()?;
        require_no_sidecars(&owner, STAGING)?;
        if persistence_entry_exists(&owner.root.path.join(STAGING))? {
            return Err(refusal());
        }
        let birth = crate::engagement::require_delegation_namespace(&owner.state_root)?;
        let (connection, database) = database_connection(&owner, DATABASE)?;
        let generation = metadata(&owner, &connection, birth)?;
        Ok(Self {
            owner,
            connection,
            database,
            generation,
            _lock: lock,
        })
    }

    fn verify(&self) -> Result<(), MachineError> {
        self.owner.revalidate()?;
        verify_database(&self.owner, &self.database, DATABASE)?;
        let birth = crate::engagement::require_delegation_namespace(&self.owner.state_root)?;
        if metadata(&self.owner, &self.connection, birth)? != self.generation {
            return Err(refusal());
        }
        Ok(())
    }

    pub fn caller_scope(&self) -> Result<crate::caller_scope::TrustedCallerScope, MachineError> {
        self.verify()?;
        crate::caller_scope::TrustedCallerScope::from_port(
            &crate::trusted_host::ConfiguredCallerPort(&self.owner),
        )
    }

    /// Pins the exact executable without launching it or editing native config.
    pub fn launch_recipe(&self) -> Result<LaunchRecipe, MachineError> {
        self.verify()?;
        let selected = &self.owner.configuration.executable;
        let file = verify_executable(selected)?;
        self.verify()?;
        Ok(LaunchRecipe {
            executable: file,
            path: selected.path.clone(),
            native_root: self.owner.configuration.native_host_root.clone(),
        })
    }

    fn request_namespace(&self, request: &Value) -> Result<Option<LookupNamespace>, MachineError> {
        if request["schema"] == "dolgorae.consumer-control/v1" {
            crate::native_consumer::Control::parse(&crate::native_consumer::encode(request)?)?;
            return Ok(None);
        }
        validate_public_request(request)?;
        let operation = request["operation"].as_str().ok_or_else(refusal)?;
        if operation == "spawn_specialist"
            || operation == "assign_specialist_task"
            || operation == "await_specialist_task"
            || operation == "collect_specialist_task"
            || operation == "cancel_specialist_task"
            || operation == "release_specialist"
            || operation == "get_managed_delegation_identity"
        {
            return Err(refusal());
        }
        if request
            .get("workspace_ref")
            .is_some_and(|value| value.as_str() != Some(&self.owner.view.workspace_id))
        {
            return Err(refusal());
        }
        let mut scope = match (request.get("scope_kind"), request.get("scope_ref")) {
            (Some(kind), Some(reference)) => Some((
                kind.as_str().ok_or_else(refusal)?,
                reference.as_str().ok_or_else(refusal)?,
            )),
            (None, None) => None,
            _ => return Err(refusal()),
        };
        if matches!(
            operation,
            "get_external_engagement" | "close_external_engagement"
        ) {
            scope = Some((
                "external_engagement",
                request["engagement_ref"].as_str().ok_or_else(refusal)?,
            ));
        }
        if operation != "lookup_operation" {
            self.owner.permit(scope)?;
        }
        if operation == "lookup_operation" {
            let namespace: LookupNamespace =
                serde_json::from_value(request["namespace"].clone()).map_err(|_| refusal())?;
            if !matches!(
                namespace.operation_kind.as_str(),
                "install_provisioning"
                    | "open_external_engagement"
                    | "hire_external_specialist"
                    | "assign_external_specialist_task"
                    | "assign_group"
                    | "assign_synthesis"
                    | "merge_group_results"
            ) {
                return Err(refusal());
            }
            let expected = crate::operation::Namespace::delegation(
                &self.owner.controller.identity,
                &self.owner.view.workspace_id,
                namespace
                    .scope_kind
                    .as_deref()
                    .zip(namespace.scope_ref.as_deref()),
                &namespace.operation_kind,
                &namespace.idempotency_key,
            )
            .map_err(|_| refusal())?;
            self.owner.permit(
                namespace
                    .scope_kind
                    .as_deref()
                    .zip(namespace.scope_ref.as_deref()),
            )?;
            if LookupNamespace::from(&expected) != namespace {
                return Err(refusal());
            }
        }
        if method_class(operation)? != MethodClass::Keyed {
            return Ok(None);
        }
        let key = request["idempotency_key"].as_str().ok_or_else(refusal)?;
        Ok(Some(
            (&crate::operation::Namespace::delegation(
                &self.owner.controller.identity,
                &self.owner.view.workspace_id,
                scope,
                operation,
                key,
            )
            .map_err(|_| refusal())?)
                .into(),
        ))
    }

    fn native_digest(&self, event: &NativeEvent) -> Result<String, MachineError> {
        digest(&(
            self.owner.configuration.route.identifier(),
            &event.session,
            &event.turn,
            &event.tool_use,
            &event.server,
            &event.tool,
        ))
    }

    fn read_record(&self, event_digest: &str) -> Result<Option<Record>, MachineError> {
        let bytes: Option<String> = self
            .connection
            .query_row(
                "SELECT record_json FROM native_calls WHERE event_digest=?1",
                [event_digest],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| refusal())?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let parsed = crate::jcs::parse(&bytes).map_err(|_| refusal())?;
        let bytes = crate::jcs::canonicalize(&parsed).map_err(|_| refusal())?;
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
        if self.owner.configuration.owner != OwnerKind::External
            || !self.owner.configuration.tools.contains(&record.event.tool)
            || (self.owner.configuration.route == HostRoute::Claude
                && record.event.server_source.as_deref() != Some("dynamic"))
        {
            return Err(refusal());
        }
        record.event.validate()?;
        normalize_tool_request(&record.event.tool, &record.request)?;
        record.context.validate().map_err(|_| refusal())?;
        let namespace = self.request_namespace(&record.request)?;
        if record.capability.len() != 32
            || record.context.state_generation != self.generation
            || record.context.host_route != self.owner.configuration.route.identifier()
            || record.context.native_event_digest != self.native_digest(&record.event)?
            || record.context.native_event_digest != event_digest
            || record.context.accepted_public_input_digest != digest(&record.request)?
            || record.context.tool_binding != record.event.tool
            || record.context.server_binding != record.event.server
            || namespace.is_some() != record.keyed.is_some()
        {
            return Err(refusal());
        }
        if let Some(keyed) = &record.keyed {
            keyed.validate().map_err(|_| refusal())?;
            if Some(&keyed.namespace) != namespace.as_ref()
                || keyed.context != record.context
                || keyed.native_session_ref != record.event.session
                || keyed.native_turn_ref != record.event.turn
                || keyed.native_tool_use_ref != record.event.tool_use
                || keyed.opaque_binding_secret_digest
                    != format!("sha256:{}", crate::jcs::sha256_hex(&record.capability))
            {
                return Err(refusal());
            }
        }
        Ok(Some(record))
    }

    fn bound(&self, record: &Record) -> BoundInvocation {
        BoundInvocation {
            owner_digest: self.owner.configuration_digest.clone(),
            owner_path: self.owner.path.clone(),
            event: record.event.clone(),
            event_digest: record.context.native_event_digest.clone(),
            context: record.context.clone(),
            request: record.request.clone(),
            namespace: record.keyed.as_ref().map(|keyed| keyed.namespace.clone()),
        }
    }

    /// Synchronous hook publication precedes any native dispatch permission.
    pub fn bind_native(
        &self,
        event: NativeEvent,
        request: Value,
    ) -> Result<(BoundInvocation, Capability), MachineError> {
        self.verify()?;
        if self.owner.configuration.owner != OwnerKind::External {
            return Err(refusal());
        }
        event.validate()?;
        if !self.owner.configuration.tools.contains(&event.tool)
            || (self.owner.configuration.route == HostRoute::Claude
                && event.server_source.as_deref() != Some("dynamic"))
        {
            return Err(refusal());
        }
        let request = normalize_tool_request(&event.tool, &request)?;
        let namespace = self.request_namespace(&request)?;
        let event_digest = self.native_digest(&event)?;
        if let Some(record) = self.read_record(&event_digest)? {
            if record.event != event || record.request != request {
                return Err(conflict());
            }
            return Ok((
                self.bound(&record),
                Capability::from_private_carrier(&record.capability)?,
            ));
        }
        let collision: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM native_calls WHERE session_ref=?1 AND tool_use_ref=?2)",
            params![event.session,event.tool_use],|row|row.get(0)).map_err(|_| refusal())?;
        if collision {
            return Err(conflict());
        }
        let capability = Capability::random()?;
        let context = TrustedInvocationContext {
            schema: "dolgorae.trusted-invocation-context/v1".to_owned(),
            invocation_ref: Uuid::now_v7(),
            host_route: self.owner.configuration.route.identifier().to_owned(),
            state_generation: self.generation,
            accepted_public_input_digest: digest(&request)?,
            native_event_digest: event_digest.clone(),
            server_binding: event.server.clone(),
            tool_binding: event.tool.clone(),
        };
        context.validate()?;
        let keyed = namespace.map(|namespace| ProtectedInvocation {
            schema: "dolgorae.protected-invocation-state/v1".to_owned(),
            context: context.clone(),
            namespace,
            native_session_ref: event.session.clone(),
            native_turn_ref: event.turn.clone(),
            native_tool_use_ref: event.tool_use.clone(),
            opaque_binding_secret_digest: format!(
                "sha256:{}",
                crate::jcs::sha256_hex(&capability.0)
            ),
            state: "prepared".to_owned(),
            operation_ref: None,
            receipt: None,
        });
        let record = Record {
            event,
            context,
            request,
            capability: capability.0.to_vec(),
            keyed,
        };
        if let Some(keyed) = &record.keyed {
            keyed.validate()?;
        }
        self.connection.execute("INSERT INTO native_calls(event_digest,record_json,session_ref,tool_use_ref) VALUES(?1,?2,?3,?4)",
            params![event_digest, String::from_utf8(canonical(&record)?).map_err(|_| refusal())?, record.event.session, record.event.tool_use]).map_err(|_| refusal())?;
        self.verify()?;
        Ok((self.bound(&record), capability))
    }

    /// Protected reconciliation selector. No public body or fallback identity.
    pub(crate) fn native_lookup(
        &self,
        session: &str,
        call: &str,
    ) -> Result<BoundInvocation, MachineError> {
        self.verify()?;
        let mut query=self.connection.prepare("SELECT event_digest FROM native_calls WHERE session_ref=?1 AND tool_use_ref=?2 LIMIT 2").map_err(|_|refusal())?;
        let keys = query
            .query_map(params![session, call], |row| row.get::<_, String>(0))
            .map_err(|_| refusal())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| refusal())?;
        if keys.len() != 1 {
            return Err(refusal());
        }
        let record = self.read_record(&keys[0])?.ok_or_else(refusal)?;
        if record.event.session != session || record.event.tool_use != call {
            return Err(refusal());
        }
        Ok(self.bound(&record))
    }

    pub fn codex_delivery(
        &self,
        session: &str,
        tool_use: &str,
        request: &Value,
    ) -> Result<BoundInvocation, MachineError> {
        self.verify()?;
        if self.owner.configuration.route != HostRoute::Codex {
            return Err(refusal());
        }
        let request = normalize_native_request(request)?;
        let mut statement = self.connection.prepare("SELECT event_digest FROM native_calls WHERE session_ref=?1 AND tool_use_ref=?2 LIMIT 2").map_err(|_| refusal())?;
        let keys = statement
            .query_map(params![session, tool_use], |row| row.get::<_, String>(0))
            .map_err(|_| refusal())?;
        let mut selected = None;
        for key in keys {
            let record = self
                .read_record(&key.map_err(|_| refusal())?)?
                .ok_or_else(refusal)?;
            if record.event.session == session && record.event.tool_use == tool_use {
                if selected.is_some() || record.request != request {
                    return Err(refusal());
                }
                selected = Some(self.bound(&record));
            }
        }
        selected.ok_or_else(refusal)
    }

    /// The native adapter removes its private carrier before passing public JSON.
    pub fn claude_delivery(
        &self,
        server_source: &str,
        event: &NativeEvent,
        capability: &Capability,
        request: &Value,
    ) -> Result<BoundInvocation, MachineError> {
        self.verify()?;
        if self.owner.configuration.route != HostRoute::Claude || server_source != "dynamic" {
            return Err(refusal());
        }
        let request = normalize_tool_request(&event.tool, request)?;
        let record = self
            .read_record(&self.native_digest(event)?)?
            .ok_or_else(refusal)?;
        if !capability.matches(&record.capability)
            || record.event != *event
            || record.request != request
        {
            return Err(refusal());
        }
        Ok(self.bound(&record))
    }

    /// Resolve the opaque native carrier without accepting model-built event fields.
    pub(crate) fn claude_carrier_delivery(
        &self,
        capability: &Capability,
        request: &Value,
    ) -> Result<BoundInvocation, MachineError> {
        self.verify()?;
        if self.owner.configuration.route != HostRoute::Claude {
            return Err(refusal());
        }
        let encoded =
            serde_json::to_string(capability.private_carrier_bytes()).map_err(|_| refusal())?;
        let mut query = self.connection.prepare(
            "SELECT event_digest FROM native_calls WHERE json_extract(record_json,'$.capability')=?1 LIMIT 2"
        ).map_err(|_| refusal())?;
        let keys = query
            .query_map([encoded], |row| row.get::<_, String>(0))
            .map_err(|_| refusal())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| refusal())?;
        if keys.len() != 1 {
            return Err(refusal());
        }
        let record = self.read_record(&keys[0])?.ok_or_else(refusal)?;
        self.claude_delivery("dynamic", &record.event, capability, request)
    }

    fn selected(&self, binding: &BoundInvocation) -> Result<Record, MachineError> {
        self.verify()?;
        let record = self
            .read_record(&binding.event_digest)?
            .ok_or_else(refusal)?;
        if record.context != binding.context
            || record.request != binding.request
            || record.keyed.as_ref().map(|keyed| &keyed.namespace) != binding.namespace.as_ref()
        {
            return Err(refusal());
        }
        Ok(record)
    }

    /// Commit before the caller invokes a product mutator. Loss on either side
    /// of that call gives the same Reconcile disposition on every later delivery.
    pub fn begin_delivery(&self, binding: &BoundInvocation) -> Result<Delivery, MachineError> {
        let record = self.selected(binding)?;
        if crate::native_consumer::is_control(&record.event.tool) {
            return Ok(Delivery::ObserveFresh);
        }
        if method_class(record.request["operation"].as_str().ok_or_else(refusal)?)?
            == MethodClass::Read
        {
            return Ok(Delivery::ObserveFresh);
        }
        let changed = self
            .connection
            .execute(
                "UPDATE native_calls SET attempted=1 WHERE event_digest=?1 AND attempted=0",
                [&binding.event_digest],
            )
            .map_err(|_| refusal())?;
        self.verify()?;
        Ok(if changed == 1 {
            Delivery::Dispatch
        } else {
            Delivery::Reconcile
        })
    }

    /// Queries the real product store itself: a caller cannot supply a receipt
    /// as authority. R/S cannot enter this K-only mirror transition.
    pub fn reconcile_keyed(
        &self,
        binding: &BoundInvocation,
    ) -> Result<crate::engagement::IntentLookup, MachineError> {
        let mut record = self.selected(binding)?;
        let keyed = record.keyed.as_mut().ok_or_else(refusal)?;
        let store = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&self.owner.state_root),
        )?;
        let lookup = if keyed.namespace.operation_kind == "assign_group" {
            let proof = binding.trusted_keyed()?;
            crate::delegation_service::lookup_native_group(
                &crate::delegation_service::DelegationScope {
                    view: &self.owner.view,
                    state_root: &self.owner.state_root,
                    controller: &self.owner.controller,
                    trusted_invocation: Some(&proof),
                },
                &self.caller_scope()?,
                &record.request,
            )?
        } else if keyed.namespace.operation_kind == "merge_group_results" {
            let proof = binding.trusted_keyed()?;
            crate::delegation_service::lookup_native_merge(
                &crate::delegation_service::DelegationScope {
                    view: &self.owner.view,
                    state_root: &self.owner.state_root,
                    controller: &self.owner.controller,
                    trusted_invocation: Some(&proof),
                },
                &record.request,
            )?
        } else if keyed.namespace.operation_kind == "assign_synthesis" {
            crate::delegation_service::lookup_operation(
                &crate::delegation_service::DelegationScope {
                    view: &self.owner.view,
                    state_root: &self.owner.state_root,
                    controller: &self.owner.controller,
                    trusted_invocation: None,
                },
                "assign_synthesis",
                keyed
                    .namespace
                    .scope_kind
                    .as_deref()
                    .zip(keyed.namespace.scope_ref.as_deref()),
                &keyed.namespace.idempotency_key,
            )?;
            store.intent_lookup_bound(&keyed.namespace, &record.context)?
        } else {
            store.intent_lookup_bound(&keyed.namespace, &record.context)?
        };
        match &lookup {
            crate::engagement::IntentLookup::Accepted(receipt) => {
                receipt.validate()?;
                receipt.verify_digest()?;
                if receipt.namespace != keyed.namespace {
                    return Err(refusal());
                }
                if keyed
                    .receipt
                    .as_ref()
                    .is_some_and(|original| original != receipt.as_ref())
                {
                    return Err(refusal());
                }
                keyed.state = "accepted".to_owned();
                keyed.operation_ref =
                    Some(Uuid::parse_str(&receipt.operation_ref).map_err(|_| refusal())?);
                keyed.receipt = Some((**receipt).clone());
            }
            crate::engagement::IntentLookup::Unresolved { operation_ref, .. } => {
                keyed.state = "unresolved".to_owned();
                keyed.operation_ref = Some(*operation_ref);
            }
            crate::engagement::IntentLookup::NotAccepted { operation_ref, .. } => {
                keyed.state = "resolved_not_accepted".to_owned();
                keyed.operation_ref = Some(*operation_ref);
            }
            crate::engagement::IntentLookup::Absent { .. } => {
                return if keyed.state == "accepted" {
                    Err(refusal())
                } else {
                    Ok(lookup)
                };
            }
        }
        keyed.validate()?;
        self.connection
            .execute(
                "UPDATE native_calls SET record_json=?2 WHERE event_digest=?1",
                params![
                    binding.event_digest,
                    String::from_utf8(canonical(&record)?).map_err(|_| refusal())?
                ],
            )
            .map_err(|_| refusal())?;
        self.verify()?;
        Ok(lookup)
    }
}

fn verify_executable(selected: &crate::trusted_host::HostExecutable) -> Result<File, MachineError> {
    use sha2::{Digest as _, Sha256};
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&selected.path)
        .map_err(|_| refusal())?;
    let before = file.metadata().map_err(|_| refusal())?;
    if !before.is_file()
        || before.mode() & 0o111 == 0
        || std::fs::canonicalize(&selected.path).map_err(|_| refusal())? != selected.path
    {
        return Err(refusal());
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|_| refusal())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    #[cfg(test)]
    crate::trusted_host::test_storage_fault();
    let after = std::fs::symlink_metadata(&selected.path).map_err(|_| refusal())?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || format!("{:x}", hasher.finalize()) != selected.sha256
    {
        return Err(refusal());
    }
    Ok(file)
}

/// Holds the verified executable descriptor; the later native launcher owns exec.
pub struct LaunchRecipe {
    executable: File,
    path: std::path::PathBuf,
    native_root: std::path::PathBuf,
}
impl LaunchRecipe {
    pub fn executable(&self) -> &File {
        &self.executable
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn native_root(&self) -> &Path {
        &self.native_root
    }
}

// The binding boundary validates checked public JSON. This does not decode or
// dispatch facade operations; the adapter still owns typed service composition.
pub(crate) fn validate_public_request(value: &Value) -> Result<(), MachineError> {
    normalize_public_request(value).map(|_| ())
}

pub(crate) fn normalize_tool_request(tool: &str, value: &Value) -> Result<Value, MachineError> {
    if crate::native_consumer::is_control(tool) {
        if value["schema"] != "dolgorae.consumer-control/v1" {
            return Err(refusal());
        }
        crate::native_consumer::Control::parse(&crate::native_consumer::encode(value)?)?;
        Ok(value.clone())
    } else if tool == crate::native_consumer::MANAGED_NATIVE
        && value["schema"] == crate::native_preparation::SCHEMA
    {
        let operation = value["operation"]
            .as_str()
            .ok_or_else(crate::native_preparation::invalid_request)?;
        let schema = &request_schemas()[4];
        let rule = schema["$defs"]
            .get(format!("{operation}_request"))
            .ok_or_else(crate::native_preparation::invalid_request)?;
        if canonical(value)?.len() > MAX_REQUEST || !schema_matches(value, rule, schema, 0) {
            return Err(crate::native_preparation::invalid_request());
        }
        Ok(value.clone())
    } else {
        normalize_public_request(value)
    }
}
fn normalize_native_request(value: &Value) -> Result<Value, MachineError> {
    if value["schema"] == "dolgorae.consumer-control/v1" {
        normalize_tool_request(crate::native_consumer::EXTERNAL_CONTROL, value)
    } else {
        normalize_public_request(value)
    }
}

/// Strict JSON/JCS input for a later native transport. Duplicate members and
/// nonrepresentable numbers are rejected before an identity can be bound.
pub fn parse_public_request(text: &str) -> Result<Value, MachineError> {
    if text.len() > MAX_REQUEST {
        return Err(refusal());
    }
    let parsed = crate::jcs::parse(text).map_err(|_| refusal())?;
    let bytes = crate::jcs::canonicalize(&parsed).map_err(|_| refusal())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
    normalize_public_request(&value)
}

fn normalize_public_request(value: &Value) -> Result<Value, MachineError> {
    let bytes = canonical(value)?;
    if bytes.len() > MAX_REQUEST {
        return Err(refusal());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
    let operation = value["operation"].as_str().ok_or_else(refusal)?;
    method_class(operation)?;
    let schemas = request_schemas();
    let definition = schemas[0]["$defs"]
        .get(format!("{operation}_request"))
        .ok_or_else(refusal)?;
    if !schema_matches(&value, definition, &schemas[0], 0) {
        return Err(refusal());
    }
    Ok(value)
}

pub(crate) fn request_schemas() -> &'static [Value; 5] {
    static SCHEMAS: std::sync::OnceLock<[Value; 5]> = std::sync::OnceLock::new();
    SCHEMAS.get_or_init(|| {
        [
            serde_json::from_str(include_str!(
                "../docs/protocol/dolgorae-external-specialist-facade-v5.schema.json"
            ))
            .expect("checked facade schema"),
            serde_json::from_str(include_str!(
                "../docs/protocol/dolgorae-v014-identities-v1.schema.json"
            ))
            .expect("checked identities schema"),
            serde_json::from_str(include_str!(
                "../docs/protocol/dolgorae-specialist-review-tool-v3.schema.json"
            ))
            .expect("checked task schema"),
            serde_json::from_str(include_str!(
                "../docs/protocol/dolgorae-review-target-v2.schema.json"
            ))
            .expect("checked change path schema"),
            serde_json::from_str(include_str!(
                "../docs/protocol/dolgorae-managed-preparation-v1.schema.json"
            ))
            .expect("checked managed preparation schema"),
        ]
    })
}

pub(crate) fn schema_matches(value: &Value, rule: &Value, base: &Value, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    if let Some(allowed) = rule.as_bool() {
        return allowed;
    }
    if rule.get("type").is_some_and(|kind| !kind.is_string()) {
        return false;
    }
    if let Some(reference) = rule["$ref"].as_str() {
        let Some((uri, pointer)) = reference.split_once('#') else {
            return false;
        };
        let Some(schema) = request_schemas()
            .iter()
            .find(|schema| uri.is_empty() && *schema == base || schema["$id"] == uri)
        else {
            return false;
        };
        let Some(target) = schema.pointer(pointer) else {
            return false;
        };
        if !schema_matches(value, target, schema, depth + 1) {
            return false;
        }
    }
    if rule.get("const").is_some_and(|expected| value != expected)
        || rule["enum"]
            .as_array()
            .is_some_and(|items| !items.contains(value))
    {
        return false;
    }
    for kind in ["allOf", "anyOf", "oneOf"] {
        if let Some(items) = rule[kind].as_array() {
            let matches = items
                .iter()
                .filter(|item| schema_matches(value, item, base, depth + 1))
                .count();
            if (kind == "allOf" && matches != items.len())
                || (kind == "anyOf" && matches == 0)
                || (kind == "oneOf" && matches != 1)
            {
                return false;
            }
        }
    }
    if let Some(condition) = rule.get("if") {
        let branch = if schema_matches(value, condition, base, depth + 1) {
            "then"
        } else {
            "else"
        };
        if rule
            .get(branch)
            .is_some_and(|branch| !schema_matches(value, branch, base, depth + 1))
        {
            return false;
        }
    }
    if rule["type"].as_str().is_some_and(|kind| !match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }) {
        return false;
    }
    if let Some(object) = value.as_object() {
        if rule["required"].as_array().is_some_and(|required| {
            required
                .iter()
                .any(|key| !key.as_str().is_some_and(|key| object.contains_key(key)))
        }) {
            return false;
        }
        for (key, member) in object {
            match rule["properties"].get(key) {
                Some(child) if !schema_matches(member, child, base, depth + 1) => return false,
                None if rule["additionalProperties"] == false => return false,
                _ => {}
            }
        }
    }
    if let Some(string) = value.as_str() {
        let chars = string.chars().count() as u64;
        if rule["minLength"].as_u64().is_some_and(|min| chars < min)
            || rule["maxLength"].as_u64().is_some_and(|max| chars > max)
            || rule["x-maxUtf8Bytes"]
                .as_u64()
                .is_some_and(|max| string.len() as u64 > max)
        {
            return false;
        }
        if let Some(pattern) = rule["pattern"].as_str() {
            // The selected frozen request/response closure uses these patterns.
            // Unknown patterns fail closed rather than being ignored.
            let valid = if pattern == "^sha256:[0-9a-f]{64}$" {
                crate::trusted_host::is_digest(string)
            } else if pattern == "^/" {
                // Explicit group input source paths use this selected pattern.
                // Filesystem containment and no-follow checks remain with the
                // group source reader after schema validation.
                string.starts_with('/')
            } else if pattern == "^[0-9a-f]{64}$" {
                string.len() == 64
                    && string
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            } else if pattern == "^(?!/)(?!.*(?:^|/)\\.\\.(?:/|$)).+$" {
                crate::delegation_identity::representable_change_path(string)
            } else if pattern
                == "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
            {
                Uuid::try_parse(string).is_ok_and(|id| {
                    id.to_string() == string
                        && id.get_version_num() == 7
                        && id.get_variant() == uuid::Variant::RFC4122
                })
            } else if pattern == "^[A-Z][A-Z0-9_]*$" {
                string
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_uppercase)
                    && string
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            } else if pattern == "^[^\\u0000-\\u001f\\u007f]+$"
                || pattern == "^[^/\\\\\\u0000-\\u001f\\u007f]+$"
            {
                !string.chars().any(|ch| {
                    ch <= '\u{1f}'
                        || ch == '\u{7f}'
                        || pattern.contains('/') && (ch == '/' || ch == '\\')
                })
            } else {
                false
            };
            if !valid {
                return false;
            }
        }
    }
    if let Some(number) = value.as_f64()
        && (rule["minimum"].as_f64().is_some_and(|min| number < min)
            || rule["maximum"].as_f64().is_some_and(|max| number > max))
    {
        return false;
    }
    if let Some(items) = value.as_array() {
        if rule["minItems"]
            .as_u64()
            .is_some_and(|min| (items.len() as u64) < min)
            || rule["maxItems"]
                .as_u64()
                .is_some_and(|max| items.len() as u64 > max)
            || rule["uniqueItems"] == true
                && items
                    .iter()
                    .enumerate()
                    .any(|(index, item)| items[..index].contains(item))
        {
            return false;
        }
        if let Some(child) = rule.get("items")
            && items
                .iter()
                .any(|item| !schema_matches(item, child, base, depth + 1))
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::delegation_identity::OperationReceipt;
    use crate::trusted_host::tests::Fixture;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn fixtures() -> Vec<Value> {
        let value: Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/successor-facade-v5-methods.valid.json"
        ))
        .unwrap();
        value["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|case| case["request"].clone())
            .collect()
    }
    fn accepted_receipt(store: &HostStore, bound: &BoundInvocation) -> OperationReceipt {
        match store.reconcile_keyed(bound).unwrap() {
            crate::engagement::IntentLookup::Accepted(receipt) => *receipt,
            state => panic!("expected original accepted receipt, received {state:?}"),
        }
    }
    fn request(operation: &str) -> Value {
        fixtures()
            .into_iter()
            .find(|request| request["operation"] == operation)
            .unwrap()
    }
    fn event(tool_use: &str) -> NativeEvent {
        NativeEvent {
            session: "native-session".to_owned(),
            turn: Some("native-turn".to_owned()),
            tool_use: tool_use.to_owned(),
            server: "dolgorae".to_owned(),
            server_source: Some("dynamic".into()),
            tool: "specialist".to_owned(),
        }
    }
    fn selected_request(store: &HostStore, operation: &str) -> Value {
        let mut value = request(operation);
        if value.get("workspace_ref").is_some() {
            value["workspace_ref"] = store.owner.view.workspace_id.clone().into();
        }
        if operation == "lookup_operation" {
            let namespace = crate::operation::Namespace::delegation(
                &store.owner.controller.identity,
                &store.owner.view.workspace_id,
                Some(("external_engagement", "engagement:fixture")),
                "assign_external_specialist_task",
                "original",
            )
            .unwrap();
            value["namespace"] = serde_json::to_value(namespace).unwrap();
        }
        value
    }

    #[test]
    fn executable_identity_corpus_pins_a_regular_canonical_unchanged_image() {
        let f = Fixture::new(HostRoute::Codex);
        let image = f.private.join("image");
        let bytes = b"private executable fixture";
        std::fs::write(&image, bytes).unwrap();
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o700)).unwrap();
        let selected = crate::trusted_host::HostExecutable {
            path: image.clone(),
            version: "fixture".into(),
            sha256: crate::jcs::sha256_hex(bytes),
        };
        verify_executable(&selected).unwrap();
        let mut bad = selected.clone();
        bad.sha256 = "0".repeat(64);
        assert!(verify_executable(&bad).is_err());
        std::fs::write(&image, b"changed executable fixture").unwrap();
        assert!(verify_executable(&selected).is_err());
        std::fs::write(&image, bytes).unwrap();
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(verify_executable(&selected).is_err());
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = f.private.join("image-alias");
        symlink(&image, &alias).unwrap();
        bad = selected.clone();
        bad.path = alias;
        assert!(verify_executable(&bad).is_err());
        let parent = f.root.join("private-alias");
        symlink(&f.private, &parent).unwrap();
        bad.path = parent.join("image");
        assert!(verify_executable(&bad).is_err());
        bad.path = f.private.clone();
        assert!(verify_executable(&bad).is_err());
        std::fs::remove_file(image).unwrap();
        assert!(verify_executable(&selected).is_err());
    }

    #[test]
    fn schema_guard_corpus_checks_closed_grammar_and_reference_failures() {
        let base = &request_schemas()[0];
        for rule in [
            serde_json::json!({"$ref":"unversioned"}),
            serde_json::json!({"$ref":"https://foreign/schema#/$defs/missing"}),
            serde_json::json!({"$ref":"#/$defs/missing"}),
            serde_json::json!({"type":["string","null"]}),
            serde_json::json!(false),
        ] {
            assert!(
                !schema_matches(&Value::Null, &rule, base, 0),
                "unsupported rule {rule}"
            );
        }
        assert!(schema_matches(
            &Value::Null,
            &serde_json::json!(true),
            base,
            0
        ));
        assert!(
            !schema_matches(&Value::Null, &serde_json::json!({}), base, 65),
            "recursion bound"
        );
        for (rule, good, bad) in [
            (
                serde_json::json!({"type":"object","additionalProperties":false,"properties":{"a":{"type":"string"}}}),
                serde_json::json!({"a":"ok"}),
                serde_json::json!({"foreign":"value"}),
            ),
            (
                serde_json::json!({"type":"array","maxItems":1}),
                serde_json::json!([]),
                serde_json::json!([1, 2]),
            ),
            (
                serde_json::json!({"type":"integer","minimum":1,"maximum":2}),
                serde_json::json!(2),
                serde_json::json!(3),
            ),
        ] {
            assert!(schema_matches(&good, &rule, base, 0));
            assert!(!schema_matches(&bad, &rule, base, 0));
        }
        for operation in ["unknown", "", "start_run"] {
            assert!(method_class(operation).is_err());
        }
    }

    #[test]
    fn kernel_database_and_executable_identity_corpus_refuses_replaced_images() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let store = f.ready();
        let database = f.private.join(DATABASE);
        let bytes = std::fs::read(&database).unwrap();
        std::fs::rename(&database, f.private.join("retained.sqlite")).unwrap();
        std::fs::write(&database, bytes).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.verify().is_err(), "open store database inode changed");
        drop(store);
        let executable = f.private.join("executable");
        let bytes = b"#!/bin/sh\nexit 0\n";
        std::fs::write(&executable, bytes).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let selected = crate::trusted_host::HostExecutable {
            path: executable.clone(),
            version: "fixture".into(),
            sha256: crate::jcs::sha256_hex(bytes),
        };
        assert!(verify_executable(&selected).is_ok());
        let swapped = executable.clone();
        let _fault = crate::trusted_host::StorageFault::at(move || {
            std::fs::rename(&swapped, swapped.with_extension("retained")).unwrap();
            std::fs::write(&swapped, bytes).unwrap();
            std::fs::set_permissions(&swapped, std::fs::Permissions::from_mode(0o700)).unwrap();
        });
        assert!(
            verify_executable(&selected).is_err(),
            "selected executable inode changed after hashing"
        );
    }

    #[test]
    fn namespace_owner_and_database_mode_corpus_refuses_foreign_inputs() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        {
            let store = f.ready();
            for (field, value) in [
                ("controller_principal", serde_json::json!("foreign")),
                ("controller_generation", serde_json::json!(2)),
                ("workspace_ref", serde_json::json!("foreign")),
                (
                    "operation_kind",
                    serde_json::json!("assign_specialist_task"),
                ),
            ] {
                let mut request = selected_request(&store, "lookup_operation");
                request["namespace"][field] = value;
                assert!(
                    store.request_namespace(&request).is_err(),
                    "foreign lookup namespace {field}"
                );
            }
        }
        let database = f.private.join(DATABASE);
        let connection = Connection::open(&database).unwrap();
        let mode: String = connection
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        connection.close().unwrap();
        assert!(
            database_connection(&f.owner(), DATABASE).is_err(),
            "host database journal mode changed"
        );
    }

    #[test]
    fn native_optional_turn_and_equal_text_calls_survive_cold_reopen_without_identity_aliasing() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let f = Fixture::new(route);
            f.complete();
            let store = f.ready();
            let value = selected_request(&store, "open_external_engagement");
            let mut first = event("first-equal-text");
            first.turn = None;
            let mut second = first.clone();
            second.tool_use = "second-equal-text".into();
            let (a, capability) = store.bind_native(first.clone(), value.clone()).unwrap();
            let (b, other_capability) = store.bind_native(second, value.clone()).unwrap();
            assert_ne!(
                a.context.invocation_ref, b.context.invocation_ref,
                "distinct native calls require distinct UUIDs"
            );
            assert_ne!(
                capability.private_carrier_bytes(),
                other_capability.private_carrier_bytes()
            );
            assert!(
                store
                    .read_record(&a.event_digest)
                    .unwrap()
                    .unwrap()
                    .keyed
                    .as_ref()
                    .unwrap()
                    .native_turn_ref
                    .is_none()
            );
            drop(store);
            let store = f.ready();
            let selected = match route {
                HostRoute::Codex => store
                    .codex_delivery(&first.session, &first.tool_use, &value)
                    .unwrap(),
                HostRoute::Claude => store
                    .claude_delivery("dynamic", &first, &capability, &value)
                    .unwrap(),
            };
            assert_eq!(selected.context(), a.context());
        }
    }

    #[test]
    fn keyed_reconciliation_corpus_preserves_absent_prepared_unresolved_and_refused_outcomes() {
        for state in ["absent", "prepared", "unresolved", "refused"] {
            let f = Fixture::new(HostRoute::Codex);
            f.complete();
            let store = f.ready();
            let value = selected_request(&store, "open_external_engagement");
            let (bound, _) = store.bind_native(event(state), value.clone()).unwrap();
            assert_eq!(store.begin_delivery(&bound).unwrap(), Delivery::Dispatch);
            if state != "absent" {
                let namespace = crate::operation::Namespace::delegation(
                    &store.owner.controller.identity,
                    &store.owner.view.workspace_id,
                    None,
                    "open_external_engagement",
                    value["idempotency_key"].as_str().unwrap(),
                )
                .unwrap();
                let envelope = crate::operation::Envelope {
                    schema:crate::operation::ENVELOPE_SCHEMA.into(),
                    request_schema:"https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/open_external_engagement_normalized_body".into(),
                    namespace,
                    trusted_invocation_ref:Some(bound.context.invocation_ref),
                    request_body:crate::operation::BytesRef { byte_length:2, sha256:format!("sha256:{}", crate::jcs::sha256_hex(b"{}")) },
                };
                let json = String::from_utf8(canonical(&envelope).unwrap()).unwrap();
                let digest = crate::operation::envelope_digest(&envelope).unwrap();
                let operation = Uuid::now_v7();
                let mut product = crate::engagement::EngagementStore::open(
                    &crate::engagement::EngagementStore::workspace_database_path(
                        &store.owner.state_root,
                    ),
                )
                .unwrap();
                product
                    .transact(|tx| {
                        crate::engagement::intent_prepare(
                            tx,
                            crate::engagement::IntentAdmission {
                                operation_ref: operation,
                                namespace: bound.namespace.as_ref().unwrap(),
                                mutation_envelope_json: &json,
                                mutation_envelope_digest: &digest,
                                trusted_invocation: Some(&bound.context),
                                expected_revision: None,
                                now_ms: 1,
                            },
                        )?;
                        if state == "unresolved" {
                            crate::engagement::intent_mark_unresolved(tx, operation, 2)?;
                        }
                        if state == "refused" {
                            crate::engagement::intent_refuse_before_acceptance(
                                tx,
                                operation,
                                "CAPABILITY_UNSUPPORTED",
                                2,
                            )?;
                        }
                        Ok(())
                    })
                    .unwrap();
            }
            let lookup = store.reconcile_keyed(&bound).unwrap();
            assert!(
                match state {
                    "absent" => matches!(lookup, crate::engagement::IntentLookup::Absent { .. }),
                    "prepared" | "unresolved" =>
                        matches!(lookup, crate::engagement::IntentLookup::Unresolved { .. }),
                    "refused" =>
                        matches!(lookup, crate::engagement::IntentLookup::NotAccepted { .. }),
                    _ => false,
                },
                "typed K lookup outcome {state}"
            );
            let record = store.read_record(&bound.event_digest).unwrap().unwrap();
            let keyed = record.keyed.as_ref().unwrap();
            assert_eq!(
                keyed.state,
                match state {
                    "refused" => "resolved_not_accepted",
                    "prepared" => "unresolved",
                    "absent" => "prepared",
                    _ => state,
                }
            );
            assert!(keyed.receipt.is_none());
            assert_eq!(store.begin_delivery(&bound).unwrap(), Delivery::Reconcile);
            let product = crate::engagement::EngagementStore::workspace_database_path(
                &store.owner.state_root,
            );
            let before = std::fs::read(&product).unwrap();
            drop(store);
            let port = crate::delegation_service::tests::FakePort::new();
            let reply = crate::specialist_facade::external_with_port(
                &f.home,
                &f.path(),
                "specialist",
                crate::specialist_facade::NativeCarrier::Codex {
                    session: "native-session",
                    call: state,
                },
                &value,
                &port,
            );
            assert!(
                crate::specialist_facade::validates(&reply, "failure"),
                "{state}: {reply}"
            );
            assert_eq!(
                reply["error"]["classification"],
                if state == "refused" {
                    "Rejected"
                } else {
                    "Unknown"
                }
            );
            assert_eq!(
                reply["error"]["effect"],
                if state == "refused" {
                    "none"
                } else {
                    "unresolved"
                }
            );
            assert_eq!(port.calls(), 0);
            assert_eq!(
                std::fs::read(&product).unwrap(),
                before,
                "adapter redispatched {state}"
            );
        }
    }

    #[test]
    fn nonkeyed_native_validation_corpus_checks_fresh_and_correlated_retained_records() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            for operation in ["get_external_engagement", "close_external_engagement"] {
                for field in [
                    "session-empty",
                    "session-long",
                    "call-empty",
                    "call-long",
                    "turn-empty",
                    "turn-long",
                    "public-extra",
                ] {
                    let f = Fixture::new(route);
                    f.complete();
                    let store = f.ready();
                    let value = selected_request(&store, operation);
                    let mut invalid = event(field);
                    match field {
                        "session-empty" => invalid.session.clear(),
                        "session-long" => invalid.session = "x".repeat(513),
                        "call-empty" => invalid.tool_use.clear(),
                        "call-long" => invalid.tool_use = "x".repeat(513),
                        "turn-empty" => invalid.turn = Some("".into()),
                        "turn-long" => invalid.turn = Some("x".repeat(513)),
                        "public-extra" => {}
                        _ => unreachable!(),
                    }
                    let mut invalid_request = value.clone();
                    if field == "public-extra" {
                        invalid_request["private_field"] = true.into();
                    }
                    assert!(
                        store
                            .bind_native(invalid.clone(), invalid_request.clone())
                            .is_err(),
                        "nonkeyed fresh {operation}/{field}"
                    );
                    assert_eq!(
                        store
                            .connection
                            .query_row("SELECT COUNT(*) FROM native_calls", [], |row| row
                                .get::<_, u64>(0))
                            .unwrap(),
                        0
                    );
                    let (bound, _) = store.bind_native(event("valid-original"), value).unwrap();
                    let mut record = store.read_record(&bound.event_digest).unwrap().unwrap();
                    record.event = invalid;
                    record.request = invalid_request;
                    record.context.native_event_digest =
                        store.native_digest(&record.event).unwrap();
                    record.context.accepted_public_input_digest = digest(&record.request).unwrap();
                    assert!(record.keyed.is_none());
                    let key = record.context.native_event_digest.clone();
                    store.connection.execute("UPDATE native_calls SET event_digest=?2,session_ref=?3,tool_use_ref=?4,record_json=?5 WHERE event_digest=?1", params![bound.event_digest, key, record.event.session, record.event.tool_use, String::from_utf8(canonical(&record).unwrap()).unwrap()]).unwrap();
                    assert!(
                        store.read_record(&key).is_err(),
                        "nonkeyed retained {operation}/{field}"
                    );
                }
            }
        }
    }

    #[test]
    fn native_event_shape_corpus_validates_each_optional_and_required_field() {
        assert!(event("valid").validate().is_ok());
        let mut absent_turn = event("absent-turn");
        absent_turn.turn = None;
        assert!(absent_turn.validate().is_ok());
        for case in 0..9 {
            let mut bad = event("invalid");
            match case {
                0 => bad.session.clear(),
                1 => bad.session = "x".repeat(513),
                2 => bad.tool_use.clear(),
                3 => bad.tool_use = "x".repeat(513),
                4 => bad.turn = Some("".into()),
                5 => bad.turn = Some("x".repeat(513)),
                6 => bad.server = "foreign".into(),
                7 => bad.tool.clear(),
                8 => bad.tool = "x".repeat(129),
                _ => unreachable!(),
            }
            assert!(bad.validate().is_err(), "native shape {case}");
        }
    }

    #[test]
    fn selected_binding_corpus_rejects_changed_proof_before_delivery_marker() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let store = f.ready();
        for case in 0..3 {
            let (mut bound, _) = store
                .bind_native(
                    event(&format!("proof-{case}")),
                    selected_request(&store, "open_external_engagement"),
                )
                .unwrap();
            match case {
                0 => bound.context.invocation_ref = Uuid::now_v7(),
                1 => bound.request["idempotency_key"] = "changed".into(),
                2 => bound.namespace.as_mut().unwrap().idempotency_key = "changed".into(),
                _ => unreachable!(),
            }
            assert!(
                store.begin_delivery(&bound).is_err(),
                "selected proof {case}"
            );
            let attempted: i64 = store
                .connection
                .query_row(
                    "SELECT attempted FROM native_calls WHERE event_digest=?1",
                    [&bound.event_digest],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(attempted, 0);
        }
    }

    #[test]
    fn frozen_request_branches_and_field_mutations_are_checked_before_binding() {
        let mut supported = 0;
        for value in fixtures() {
            if method_class(value["operation"].as_str().unwrap()).is_err() {
                continue;
            }
            supported += 1;
            assert!(
                validate_public_request(&value).is_ok(),
                "{}",
                value["operation"]
            );
            for key in value.as_object().unwrap().keys() {
                let mut missing = value.clone();
                missing.as_object_mut().unwrap().remove(key);
                assert!(
                    validate_public_request(&missing).is_err(),
                    "missing {}.{key}",
                    value["operation"]
                );
            }
            for key in value.as_object().unwrap().keys() {
                let mut wrong = value.clone();
                wrong[key] = match &value[key] {
                    Value::String(_) => serde_json::json!(false),
                    Value::Bool(_) => serde_json::json!("false"),
                    Value::Array(_) | Value::Object(_) => serde_json::json!("wrong"),
                    _ => serde_json::json!([]),
                };
                assert!(
                    validate_public_request(&wrong).is_err(),
                    "wrong type {}.{key}",
                    value["operation"]
                );
            }
            let mut bad_schema = value.clone();
            bad_schema["schema"] = "foreign".into();
            assert!(validate_public_request(&bad_schema).is_err());
            if value.get("scope_kind").is_some() {
                let mut bad = value.clone();
                bad["scope_kind"] = "foreign".into();
                assert!(validate_public_request(&bad).is_err());
            }
            for field in [
                "controller_file",
                "trusted_invocation",
                "_dolgorae_binding",
                "_dolgorae_host_binding",
                "caller_scope",
                "unknown",
            ] {
                let mut extra = value.clone();
                extra[field] = "private".into();
                assert!(
                    validate_public_request(&extra).is_err(),
                    "extra {}.{field}",
                    value["operation"]
                );
            }
        }
        assert_eq!(supported, 25);
        let duplicate = r#"{"schema":"dolgorae.external-specialist-request/v5","operation":"open_external_engagement","operation":"open_external_engagement","workspace_ref":"workspace","idempotency_key":"key","expected_revision":null}"#;
        assert!(parse_public_request(duplicate).is_err());
        let mut fractional = request("hire_external_specialist");
        fractional["expected_revision"] = serde_json::json!(1.0);
        assert_eq!(
            normalize_public_request(&fractional).unwrap()["expected_revision"],
            serde_json::json!(1)
        );
        let mut bad = request("hire_external_specialist");
        bad["agent_configuration_digest"] = "sha256:BAD".into();
        assert!(validate_public_request(&bad).is_err());
        for value in [
            serde_json::json!(-1),
            serde_json::json!(MAX_INTEGER + 1),
            serde_json::json!(1.5),
        ] {
            let mut bad = request("hire_external_specialist");
            bad["expected_revision"] = value;
            assert!(validate_public_request(&bad).is_err());
        }
        for (rule, good, bad) in [
            (
                serde_json::json!({"type":"string","minLength":1}),
                serde_json::json!("a"),
                serde_json::json!(""),
            ),
            (
                serde_json::json!({"type":"string","maxLength":2}),
                serde_json::json!("ab"),
                serde_json::json!("abc"),
            ),
            (
                serde_json::json!({"type":"string","enum":["a","b"]}),
                serde_json::json!("a"),
                serde_json::json!("c"),
            ),
            (
                serde_json::json!({"type":"array","minItems":1}),
                serde_json::json!([1]),
                serde_json::json!([]),
            ),
            (
                serde_json::json!({"type":"array","maxItems":1}),
                serde_json::json!([1]),
                serde_json::json!([1, 2]),
            ),
            (
                serde_json::json!({"type":"array","items":{"type":"string"}}),
                serde_json::json!(["a"]),
                serde_json::json!([1]),
            ),
            (
                serde_json::json!({"oneOf":[{"type":"string"},{"const":1}]}),
                serde_json::json!(1),
                serde_json::json!(false),
            ),
            (
                serde_json::json!({"allOf":[{"type":"string"},{"enum":["a"]}]}),
                serde_json::json!("a"),
                serde_json::json!("b"),
            ),
            (
                serde_json::json!({"type":"string","pattern":"unsupported"}),
                Value::Null,
                serde_json::json!("anything"),
            ),
        ] {
            if !good.is_null() {
                assert!(schema_matches(&good, &rule, &request_schemas()[0], 0));
            }
            assert!(!schema_matches(&bad, &rule, &request_schemas()[0], 0));
        }
        let schemas = request_schemas();
        for (rule, good, bad) in [
            (
                serde_json::json!({"type":"string","minLength":1,"maxLength":2,"x-maxUtf8Bytes":2}),
                serde_json::json!("é"),
                serde_json::json!("éé"),
            ),
            (
                serde_json::json!({"type":"array","minItems":1,"maxItems":2,"uniqueItems":true,"items":{"type":"string"}}),
                serde_json::json!(["a"]),
                serde_json::json!(["a", "a"]),
            ),
            (
                serde_json::json!({"if":{"const":true},"then":{"type":"boolean"},"else":{"const":null}}),
                serde_json::json!(true),
                serde_json::json!(false),
            ),
        ] {
            assert!(schema_matches(&good, &rule, &schemas[0], 0));
            assert!(!schema_matches(&bad, &rule, &schemas[0], 0));
        }
    }

    // Keep every context within its frozen UTF-8 limit while varying aggregate
    // size. Recompute the supplied byte references instead of enlarging a field.
    pub(crate) fn aggregate_request(target: usize) -> Value {
        let mut value = request("assign_external_specialist_task");
        let contexts = (0..64)
            .map(|index| {
                serde_json::json!({
                    "id": format!("context-{index}"), "content": "x", "provenance": "fixture"
                })
            })
            .collect::<Vec<_>>();
        value["assignment"]["task_body"]["contexts"] = contexts.into();
        let refresh = |value: &mut Value| {
            let body = &value["assignment"]["task_body"];
            let refs = body["contexts"].as_array().unwrap().iter().map(|context| {
                let bytes = context["content"].as_str().unwrap().as_bytes();
                serde_json::json!({"kind":"accepted_context", "identity":context["id"],
                    "content":{"byte_length":bytes.len(), "sha256":format!("sha256:{}", crate::jcs::sha256_hex(bytes))},
                    "media_type":"text/plain"})
            }).collect::<Vec<_>>();
            let bytes = canonical(body).unwrap();
            value["assignment"]["accepted_task_identity"]["contexts"] = refs.into();
            value["assignment"]["accepted_task_identity"]["task_body"] = serde_json::json!({
                "byte_length":bytes.len(), "sha256":format!("sha256:{}", crate::jcs::sha256_hex(&bytes))});
        };
        refresh(&mut value);
        // Decimal byte lengths change the overhead, so converge after filling.
        for _ in 0..8 {
            let size = canonical(&value).unwrap().len();
            if size == target {
                return value;
            }
            let contexts = value["assignment"]["task_body"]["contexts"]
                .as_array_mut()
                .unwrap();
            let mut remaining = size.abs_diff(target);
            for context in contexts {
                let text = context["content"].as_str().unwrap();
                let change = remaining.min(if size < target {
                    262_144 - text.len()
                } else {
                    text.len() - 1
                });
                let length = if size < target {
                    text.len() + change
                } else {
                    text.len() - change
                };
                context["content"] = "x".repeat(length).into();
                remaining -= change;
                if remaining == 0 {
                    break;
                }
            }
            assert_eq!(
                remaining, 0,
                "target exceeds schema-valid aggregate capacity"
            );
            refresh(&mut value);
        }
        panic!("aggregate request did not converge");
    }

    #[test]
    fn request_size_corpus_preserves_valid_aggregate_and_private_record_boundaries() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let store = f.ready();
        for size in [
            2 * 1024 * 1024 + 1,
            MAX_REQUEST - 1,
            MAX_REQUEST + 1,
            MAX_REQUEST,
        ] {
            let value = aggregate_request(size);
            let schemas = request_schemas();
            assert!(schema_matches(
                &value,
                &schemas[0]["$defs"]["assign_external_specialist_task_request"],
                &schemas[0],
                0
            ));
            let bytes = canonical(&value).unwrap();
            assert_eq!(bytes.len(), size);
            let text = std::str::from_utf8(&bytes).unwrap();
            if size > MAX_REQUEST {
                assert!(
                    parse_public_request(text).is_err(),
                    "raw request exceeds limit"
                );
                assert!(
                    store.bind_native(event("oversized"), value).is_err(),
                    "canonical request exceeds limit"
                );
                continue;
            }
            assert_eq!(parse_public_request(text).unwrap(), value);
            if size == MAX_REQUEST {
                let padded = format!("{text} ");
                assert!(
                    parse_public_request(&padded).is_err(),
                    "raw limit includes whitespace"
                );
                let (bound, _) = store.bind_native(event("maximum"), value.clone()).unwrap();
                let length: usize = store.connection.query_row("SELECT length(CAST(record_json AS BLOB)) FROM native_calls WHERE event_digest=?1", [&bound.event_digest], |row| row.get(0)).unwrap();
                assert!(
                    length > MAX_REQUEST,
                    "private metadata exceeds public limit"
                );
                drop(store);
                let reopened = f.ready();
                let selected = reopened
                    .codex_delivery("native-session", "maximum", &value)
                    .unwrap();
                assert_eq!(selected.context(), bound.context());
                assert_eq!(
                    reopened.begin_delivery(&selected).unwrap(),
                    Delivery::Dispatch
                );
                assert_eq!(
                    reopened.begin_delivery(&selected).unwrap(),
                    Delivery::Reconcile
                );
                return;
            }
        }
    }

    #[test]
    fn native_kr_s_corpus_retains_identity_without_synthetic_product_records() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let f = Fixture::new(route);
            f.complete();
            let store = f.ready();
            let mut count = [0; 3];
            for mut value in fixtures() {
                let operation = value["operation"].as_str().unwrap().to_owned();
                let Ok(class) = method_class(&operation) else {
                    continue;
                };
                if !operation.contains("external")
                    && !matches!(
                        operation.as_str(),
                        "validate_provisioning"
                            | "install_provisioning"
                            | "lookup_operation"
                            | "assign_group"
                            | "await_group"
                            | "collect_group"
                    )
                    && !(matches!(
                        operation.as_str(),
                        "assign_synthesis" | "merge_group_results"
                    ) && value["scope_kind"] == "external_engagement")
                {
                    assert!(store.bind_native(event(&operation), value).is_err());
                    continue;
                }
                value = selected_request(&store, &operation);
                let native = event(&operation);
                let (bound, capability) = store.bind_native(native.clone(), value.clone()).unwrap();
                let (again, original) = store.bind_native(native.clone(), value.clone()).unwrap();
                assert_eq!(bound.context(), again.context());
                assert_eq!(
                    capability.private_carrier_bytes(),
                    original.private_carrier_bytes()
                );
                assert_eq!(bound.context.invocation_ref.get_version_num(), 7);
                let record = store.read_record(&bound.event_digest).unwrap().unwrap();
                assert_eq!(record.keyed.is_some(), class == MethodClass::Keyed);
                if let Some(keyed) = &record.keyed {
                    assert_eq!(keyed.namespace.idempotency_key, value["idempotency_key"]);
                    assert_eq!(keyed.state, "prepared");
                    assert!(keyed.receipt.is_none());
                    assert!(keyed.operation_ref.is_none());
                    bound.trusted_keyed().unwrap();
                } else {
                    assert!(bound.trusted_keyed().is_err());
                    assert!(store.reconcile_keyed(&bound).is_err());
                }
                let selected = if route == HostRoute::Codex {
                    store
                        .codex_delivery(&native.session, &native.tool_use, &value)
                        .unwrap()
                } else {
                    store
                        .claude_delivery("dynamic", &native, &capability, &value)
                        .unwrap()
                };
                assert_eq!(selected.context(), bound.context());
                let first = store.begin_delivery(&selected).unwrap();
                let second = store.begin_delivery(&selected).unwrap();
                match class {
                    MethodClass::Keyed => {
                        count[0] += 1;
                        assert_eq!(first, Delivery::Dispatch);
                        assert_eq!(second, Delivery::Reconcile);
                        if matches!(
                            operation.as_str(),
                            "assign_group" | "merge_group_results" | "assign_synthesis"
                        ) {
                            // This grammar-only corpus has no retained aggregate
                            // owner. Group reconciliation must refuse it even
                            // for an unused key; real owner paths are separate.
                            assert!(store.reconcile_keyed(&bound).is_err());
                        } else {
                            assert!(matches!(
                                store.reconcile_keyed(&bound).unwrap(),
                                crate::engagement::IntentLookup::Absent { .. }
                            ));
                        }
                    }
                    MethodClass::Read => {
                        count[1] += 1;
                        assert_eq!(first, Delivery::ObserveFresh);
                        assert_eq!(second, Delivery::ObserveFresh);
                    }
                    MethodClass::State => {
                        count[2] += 1;
                        assert_eq!(first, Delivery::Dispatch);
                        assert_eq!(second, Delivery::Reconcile);
                    }
                }
            }
            assert_eq!(count, [7, 8, 3]);
            assert_eq!(
                store
                    .connection
                    .query_row("SELECT COUNT(*) FROM native_calls", [], |row| row
                        .get::<_, u64>(0))
                    .unwrap(),
                18
            );
            let product = crate::engagement::EngagementStore::open_read_only(
                &crate::engagement::EngagementStore::workspace_database_path(
                    &store.owner.state_root,
                ),
            )
            .unwrap();
            assert_eq!(
                product
                    .connection()
                    .query_row("SELECT COUNT(*) FROM delegation_operations", [], |row| row
                        .get::<_, u64>(
                        0
                    ))
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn native_authentication_ambiguity_and_input_corpus_never_rebinds() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let f = Fixture::new(route);
            f.complete();
            let store = f.ready();
            let value = selected_request(&store, "open_external_engagement");
            let native = event("original");
            let (bound, cap) = store.bind_native(native.clone(), value.clone()).unwrap();
            let mut changed = value.clone();
            changed["idempotency_key"] = "changed".into();
            assert_eq!(
                store
                    .bind_native(native.clone(), changed.clone())
                    .err()
                    .unwrap()
                    .code,
                "IDEMPOTENCY_CONFLICT"
            );
            assert!(
                store
                    .codex_delivery("missing", &native.tool_use, &value)
                    .is_err()
            );
            assert!(
                store
                    .codex_delivery(&native.session, "missing", &value)
                    .is_err()
            );
            assert!(
                store
                    .codex_delivery(&native.session, &native.tool_use, &changed)
                    .is_err()
            );
            assert!(
                store
                    .claude_delivery("foreign", &native, &cap, &value)
                    .is_err()
            );
            assert!(
                store
                    .claude_delivery("dynamic", &native, &Capability([0; 32]), &value)
                    .is_err()
            );
            assert!(
                store
                    .claude_delivery("dynamic", &native, &cap, &changed)
                    .is_err()
            );
            for width in 0..65 {
                assert_eq!(
                    Capability::from_private_carrier(&vec![0; width]).is_ok(),
                    width == 32
                );
            }
            if route == HostRoute::Claude {
                assert!(
                    store
                        .codex_delivery(&native.session, &native.tool_use, &value)
                        .is_err(),
                    "Codex metadata cannot authenticate Claude calls"
                );
            }
            for case in 0..5 {
                let mut n = event(&format!("invalid-native-{case}"));
                match case {
                    0 => n.session = "".into(),
                    1 => n.turn = Some("".into()),
                    2 => n.tool_use = "".into(),
                    3 => n.server = "foreign".into(),
                    4 => n.tool = "unconfigured".into(),
                    _ => unreachable!(),
                }
                assert!(
                    store.bind_native(n, value.clone()).is_err(),
                    "native field {case}"
                );
            }
            let mut same_call = native.clone();
            same_call.turn = Some("another-turn".into());
            assert_eq!(
                store
                    .bind_native(same_call, value.clone())
                    .err()
                    .unwrap()
                    .code,
                "IDEMPOTENCY_CONFLICT"
            );
            if route == HostRoute::Codex {
                assert!(
                    store
                        .codex_delivery(&native.session, &native.tool_use, &value)
                        .is_ok()
                );
            } else {
                let mut untrusted = event("invalid-origin");
                untrusted.server_source = Some("project".into());
                assert!(store.bind_native(untrusted, value.clone()).is_err());
                let mut missing = event("missing-origin");
                missing.server_source = None;
                assert!(store.bind_native(missing, value.clone()).is_err());
            }
            let retained = store.read_record(&bound.event_digest).unwrap().unwrap();
            assert_eq!(retained.context, bound.context);
            let mut foreign = value;
            foreign["workspace_ref"] = "foreign".into();
            assert!(
                store
                    .bind_native(event("foreign-workspace"), foreign)
                    .is_err()
            );
        }
    }

    #[test]
    fn scope_targets_and_owner_modes_are_checked_for_every_method_family() {
        let mut f = Fixture::new(HostRoute::Codex);
        f.draft.allowed_scopes[1].reference = Some("only-this-engagement".into());
        f.complete();
        let mut store = f.ready();
        for operation in [
            "get_external_engagement",
            "close_external_engagement",
            "hire_external_specialist",
            "await_external_specialist_task",
            "lookup_operation",
        ] {
            let value = selected_request(&store, operation);
            assert!(
                store.bind_native(event(operation), value).is_err(),
                "scope {operation}"
            );
        }
        let value = selected_request(&store, "open_external_engagement");
        store.owner.configuration.owner = OwnerKind::Managed {
            session_ref: Uuid::now_v7(),
        };
        // The immutable owner record is also rechecked; a mutable in-memory claim is insufficient.
        assert!(store.bind_native(event("managed"), value).is_err());
    }

    #[test]
    fn cold_delivery_markers_and_generation_loss_require_reconciliation() {
        for operation in [
            "open_external_engagement",
            "close_external_engagement",
            "get_external_engagement",
        ] {
            let f = Fixture::new(HostRoute::Codex);
            f.complete();
            let store = f.ready();
            let value = selected_request(&store, operation);
            let native = event(operation);
            let (binding, _) = store.bind_native(native.clone(), value.clone()).unwrap();
            let class = method_class(operation).unwrap();
            assert_eq!(
                store.begin_delivery(&binding).unwrap(),
                if class == MethodClass::Read {
                    Delivery::ObserveFresh
                } else {
                    Delivery::Dispatch
                }
            );
            drop(store);
            let store = f.ready();
            let bound = store
                .codex_delivery(&native.session, &native.tool_use, &value)
                .unwrap();
            assert_eq!(bound.context(), binding.context());
            assert_eq!(
                store.begin_delivery(&bound).unwrap(),
                if class == MethodClass::Read {
                    Delivery::ObserveFresh
                } else {
                    Delivery::Reconcile
                }
            );
            store
                .connection
                .execute(
                    "UPDATE metadata SET generation=generation%9007199254740991+1",
                    [],
                )
                .unwrap();
            assert!(store.begin_delivery(&bound).is_err());
            drop(store);
            let store = f.ready();
            assert!(
                store
                    .codex_delivery(&native.session, &native.tool_use, &value)
                    .is_err()
            );
            assert!(store.bind_native(native, value).is_err());
            drop(store);
            std::fs::remove_file(f.private.join(DATABASE)).unwrap();
            assert!(f.owner().ready().is_err());
            assert!(!f.private.join(DATABASE).exists());
        }
    }

    #[test]
    fn initialization_remnant_corpus_preserves_interrupted_persistence() {
        let names: Vec<String> = std::iter::once(STAGING.to_owned())
            .chain([DATABASE, STAGING].into_iter().flat_map(|name| {
                ["-journal", "-wal", "-shm"].map(|suffix| format!("{name}{suffix}"))
            }))
            .collect();
        let snapshot = |root: &Path| {
            let mut entries: Vec<_> = std::fs::read_dir(root)
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    let m = std::fs::symlink_metadata(&path).unwrap();
                    let content = if m.is_file() {
                        crate::jcs::sha256_hex(&std::fs::read(&path).unwrap())
                    } else if m.is_symlink() {
                        std::fs::read_link(&path).unwrap().display().to_string()
                    } else {
                        "directory".into()
                    };
                    (
                        path.file_name().unwrap().to_owned(),
                        m.ino(),
                        m.mode(),
                        m.nlink(),
                        content,
                    )
                })
                .collect();
            entries.sort();
            entries
        };
        for final_state in ["absent", "complete", "empty", "corrupt"] {
            let f = Fixture::new(HostRoute::Codex);
            f.complete();
            let database = f.private.join(DATABASE);
            match final_state {
                "absent" => std::fs::remove_file(&database).unwrap(),
                "empty" => std::fs::write(&database, b"").unwrap(),
                "corrupt" => std::fs::write(&database, b"corrupt database").unwrap(),
                _ => {}
            }
            for mask in 0..(1 << names.len()) {
                if mask == 0 && matches!(final_state, "absent" | "complete") {
                    continue;
                }
                for (bit, name) in names.iter().enumerate() {
                    if mask & (1 << bit) != 0 {
                        std::fs::write(f.private.join(name), b"interrupted persistence").unwrap();
                    }
                }
                let before = snapshot(&f.private);
                assert!(
                    f.initialize().is_err(),
                    "remnant refusal {final_state}/{mask}"
                );
                assert_eq!(
                    snapshot(&f.private),
                    before,
                    "init modified {final_state}/{mask}"
                );
                assert!(f.owner().ready().is_err(), "partial state became ready");
                assert_eq!(
                    snapshot(&f.private),
                    before,
                    "open modified {final_state}/{mask}"
                );
                for (bit, name) in names.iter().enumerate() {
                    if mask & (1 << bit) != 0 {
                        std::fs::remove_file(f.private.join(name)).unwrap();
                    }
                }
            }
        }
        for name in &names {
            for shape in ["empty", "symlink", "directory"] {
                let f = Fixture::new(HostRoute::Codex);
                f.create().unwrap();
                f.namespace();
                let path = f.private.join(name);
                match shape {
                    "empty" => std::fs::write(&path, b"").unwrap(),
                    "symlink" => symlink("missing-remnant", &path).unwrap(),
                    "directory" => std::fs::create_dir(&path).unwrap(),
                    _ => unreachable!(),
                }
                let before = snapshot(&f.private);
                assert!(f.initialize().is_err(), "remnant shape {name}/{shape}");
                assert_eq!(snapshot(&f.private), before, "remnant shape modified");
            }
        }
        let f = Fixture::new(HostRoute::Codex);
        f.create().unwrap();
        f.namespace();
        f.initialize().unwrap();
        assert!(persistence_entry_exists(&f.private.join("controller.json/sidecar")).is_err());
        assert!(!persistence_entry_exists(&f.private.join("missing-sidecar")).unwrap());
        let before = snapshot(&f.private);
        f.initialize().unwrap();
        drop(f.owner().ready().unwrap());
        assert_eq!(
            snapshot(&f.private),
            before,
            "complete initialization changed state"
        );
    }

    #[test]
    fn protected_database_alias_and_corruption_corpus_never_repairs() {
        for case in 0..7 {
            let f = Fixture::new(HostRoute::Codex);
            f.complete();
            let path = f.private.join(DATABASE);
            let before = std::fs::read(&path).unwrap();
            match case {
                0 => {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap()
                }
                1 => {
                    std::fs::hard_link(&path, f.private.join("alias")).unwrap();
                }
                2 => {
                    std::fs::rename(&path, f.private.join("original")).unwrap();
                    symlink(f.private.join("original"), &path).unwrap();
                }
                3 => {
                    std::fs::write(
                        f.private.join(format!("{DATABASE}-journal")),
                        b"interrupted",
                    )
                    .unwrap();
                }
                4 => {
                    std::fs::write(&path, b"corrupt").unwrap();
                }
                5 => {
                    let c = Connection::open(&path).unwrap();
                    c.execute(
                        "UPDATE metadata SET schema_version=1, birth_id='foreign'",
                        [],
                    )
                    .unwrap();
                }
                6 => {
                    std::fs::write(f.private.join(STAGING), b"unpublished database").unwrap();
                }
                _ => unreachable!(),
            }
            let changed = std::fs::read(&path).unwrap();
            assert!(f.owner().ready().is_err(), "database case {case}");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                changed,
                "database case {case} repaired state"
            );
            if case < 4 {
                assert_eq!(changed, before);
            }
        }
    }

    #[test]
    fn accepted_product_receipt_recovers_a_lagging_native_mirror() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let store = f.ready();
        let value = selected_request(&store, "open_external_engagement");
        let native = event("accepted");
        let (binding, _) = store.bind_native(native.clone(), value.clone()).unwrap();
        assert_eq!(store.begin_delivery(&binding).unwrap(), Delivery::Dispatch);
        let product_database =
            crate::engagement::EngagementStore::workspace_database_path(&store.owner.state_root);
        let before_product_acceptance = f.private.join("before-product-acceptance.sqlite");
        Connection::open(&product_database)
            .unwrap()
            .execute(
                "VACUUM INTO ?1",
                [before_product_acceptance.to_str().unwrap()],
            )
            .unwrap();
        std::fs::set_permissions(
            &before_product_acceptance,
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let proof = binding.trusted_keyed().unwrap();
        let scope = crate::delegation_service::DelegationScope {
            view: &store.owner.view,
            state_root: &store.owner.state_root,
            controller: &store.owner.controller,
            trusted_invocation: Some(&proof),
        };
        let (engagement, receipt) = crate::delegation_service::open_external_engagement(
            &scope,
            value["idempotency_key"].as_str().unwrap(),
            None,
        )
        .unwrap();
        let product_count_before = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&store.owner.state_root),
        )
        .unwrap()
        .connection()
        .query_row("SELECT COUNT(*) FROM delegation_operations", [], |row| {
            row.get::<_, u64>(0)
        })
        .unwrap();
        assert!(
            crate::delegation_service::open_external_engagement(&scope, "foreign-key", None)
                .is_err()
        );
        assert_eq!(
            crate::engagement::EngagementStore::open_read_only(
                &crate::engagement::EngagementStore::workspace_database_path(
                    &store.owner.state_root
                )
            )
            .unwrap()
            .connection()
            .query_row("SELECT COUNT(*) FROM delegation_operations", [], |row| row
                .get::<_, u64>(
                0
            ))
            .unwrap(),
            product_count_before
        );

        let product = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&store.owner.state_root),
        )
        .unwrap();
        let persisted: String = product
            .connection()
            .query_row(
                "SELECT trusted_invocation_json FROM delegation_operations WHERE operation_ref=?1",
                [&receipt.operation_ref],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<TrustedInvocationContext>(&persisted).unwrap(),
            binding.context
        );
        let record = store.read_record(&binding.event_digest).unwrap().unwrap();
        assert_eq!(record.keyed.as_ref().unwrap().state, "prepared");
        // Simulate loss after the product commit, before mirror publication.
        drop(product);
        drop(store);
        let store = f.ready();
        let bound = store
            .codex_delivery(&native.session, &native.tool_use, &value)
            .unwrap();
        assert_eq!(store.begin_delivery(&bound).unwrap(), Delivery::Reconcile);
        assert_eq!(accepted_receipt(&store, &bound), receipt);
        assert_eq!(accepted_receipt(&store, &bound), receipt);
        let record = store.read_record(&bound.event_digest).unwrap().unwrap();
        assert_eq!(record.keyed.as_ref().unwrap().state, "accepted");
        assert_eq!(
            record
                .keyed
                .as_ref()
                .unwrap()
                .receipt
                .as_ref()
                .unwrap()
                .accepted_subjects
                .engagement_ref
                .as_deref(),
            Some(engagement.engagement_ref.as_str())
        );
        let original: String = store
            .connection
            .query_row(
                "SELECT record_json FROM native_calls WHERE event_digest=?1",
                [&bound.event_digest],
                |row| row.get(0),
            )
            .unwrap();
        let mut changed: Record = serde_json::from_str(&original).unwrap();
        let mirrored = changed.keyed.as_mut().unwrap().receipt.as_mut().unwrap();
        mirrored.accepted_revision += 1;
        mirrored.receipt_digest = mirrored.compute_digest().unwrap();
        changed.keyed.as_ref().unwrap().validate().unwrap();
        store
            .connection
            .execute(
                "UPDATE native_calls SET record_json=?2 WHERE event_digest=?1",
                params![
                    bound.event_digest,
                    String::from_utf8(canonical(&changed).unwrap()).unwrap()
                ],
            )
            .unwrap();
        assert!(
            store.reconcile_keyed(&bound).is_err(),
            "retained accepted receipt changed"
        );
        store
            .connection
            .execute(
                "UPDATE native_calls SET record_json=?2 WHERE event_digest=?1",
                params![bound.event_digest, original],
            )
            .unwrap();
        let proof = bound.trusted_keyed().unwrap();
        let product = crate::engagement::EngagementStore::open(
            &crate::engagement::EngagementStore::workspace_database_path(&store.owner.state_root),
        )
        .unwrap();
        for case in 0..9 {
            let mut wrong = proof.context().clone();
            match case {
                0 => wrong.invocation_ref = Uuid::now_v7(),
                1 => wrong.host_route = "claude-code-2.1.295".into(),
                2 => wrong.state_generation = wrong.state_generation % MAX_INTEGER + 1,
                3 => wrong.accepted_public_input_digest = format!("sha256:{}", "a".repeat(64)),
                4 => wrong.native_event_digest = format!("sha256:{}", "b".repeat(64)),
                5 => wrong.tool_binding = "other-tool".into(),
                6 => wrong.server_binding = "foreign".into(),
                7 => wrong.schema = "foreign".into(),
                8 => wrong.invocation_ref = Uuid::nil(),
                _ => unreachable!(),
            }
            product.connection().execute("UPDATE delegation_operations SET trusted_invocation_json=?1 WHERE operation_ref=?2",
                params![serde_json::to_string(&wrong).unwrap(),receipt.operation_ref]).unwrap();
            assert!(
                store.reconcile_keyed(&bound).is_err(),
                "product context mutation {case}"
            );
            assert_eq!(
                store
                    .read_record(&bound.event_digest)
                    .unwrap()
                    .unwrap()
                    .keyed
                    .as_ref()
                    .unwrap()
                    .receipt
                    .as_ref(),
                Some(&receipt)
            );
        }
        product.connection().execute("UPDATE delegation_operations SET trusted_invocation_json=?1 WHERE operation_ref=?2",
            params![serde_json::to_string(proof.context()).unwrap(),receipt.operation_ref]).unwrap();
        drop(product);
        let mut wrong = proof.context().clone();
        wrong.invocation_ref = Uuid::now_v7();
        let product = crate::engagement::EngagementStore::open(
            &crate::engagement::EngagementStore::workspace_database_path(&store.owner.state_root),
        )
        .unwrap();
        product.connection().execute("UPDATE delegation_operations SET trusted_invocation_json=?1 WHERE operation_ref=?2",
            params![serde_json::to_string(&wrong).unwrap(),receipt.operation_ref]).unwrap();
        assert!(store.reconcile_keyed(&bound).is_err());
        assert_eq!(
            store
                .read_record(&bound.event_digest)
                .unwrap()
                .unwrap()
                .keyed
                .as_ref()
                .unwrap()
                .receipt
                .as_ref(),
            Some(&receipt)
        );
        drop(product);
        // The snapshot includes committed WAL content. Quiesce this isolated
        // database before restoring it; never change its immutable-row triggers.
        let connection = Connection::open(&product_database).unwrap();
        let (busy, _, _): (u32, u32, u32) = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(busy, 0);
        drop(connection);
        for suffix in ["-wal", "-shm"] {
            let path = product_database.with_file_name(format!(
                "{}{suffix}",
                product_database.file_name().unwrap().to_str().unwrap()
            ));
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
        std::fs::write(
            &product_database,
            std::fs::read(before_product_acceptance).unwrap(),
        )
        .unwrap();
        let product =
            crate::engagement::EngagementStore::open_read_only(&product_database).unwrap();
        assert!(matches!(
            product
                .intent_lookup(bound.namespace.as_ref().unwrap())
                .unwrap(),
            crate::engagement::IntentLookup::Absent { .. }
        ));
        assert!(
            store.reconcile_keyed(&bound).is_err(),
            "accepted mirror cannot replace missing product acceptance"
        );
    }

    #[test]
    fn retained_claude_origin_corpus_requires_dynamic_server_provenance() {
        let f = Fixture::new(HostRoute::Claude);
        f.complete();
        let store = f.ready();
        let (bound, _) = store
            .bind_native(
                event("retained-origin"),
                selected_request(&store, "open_external_engagement"),
            )
            .unwrap();
        let original: String = store
            .connection
            .query_row(
                "SELECT record_json FROM native_calls WHERE event_digest=?1",
                [&bound.event_digest],
                |row| row.get(0),
            )
            .unwrap();
        for origin in [
            Value::Null,
            serde_json::json!("project"),
            serde_json::json!("user"),
        ] {
            let mut value: Value = serde_json::from_str(&original).unwrap();
            value["event"]["server_source"] = origin;
            store
                .connection
                .execute(
                    "UPDATE native_calls SET record_json=?2 WHERE event_digest=?1",
                    params![
                        bound.event_digest,
                        String::from_utf8(canonical(&value).unwrap()).unwrap()
                    ],
                )
                .unwrap();
            assert!(
                store.read_record(&bound.event_digest).is_err(),
                "retained Claude origin"
            );
        }
    }

    #[test]
    fn native_record_corruption_corpus_cannot_supply_delivery_authority() {
        for operation in [
            "open_external_engagement",
            "get_external_engagement",
            "close_external_engagement",
        ] {
            let f = Fixture::new(HostRoute::Codex);
            f.complete();
            let store = f.ready();
            let request = selected_request(&store, operation);
            let native = event(operation);
            let (bound, _) = store.bind_native(native, request).unwrap();
            let original: String = store
                .connection
                .query_row(
                    "SELECT record_json FROM native_calls WHERE event_digest=?1",
                    [&bound.event_digest],
                    |row| row.get(0),
                )
                .unwrap();
            for pointer in [
                "",
                "/event",
                "/context",
                "/keyed",
                "/keyed/context",
                "/keyed/namespace",
            ] {
                let mut value: Value = serde_json::from_str(&original).unwrap();
                let Some(object) = value.pointer_mut(pointer).and_then(Value::as_object_mut) else {
                    continue;
                };
                object.insert("unexpected".into(), true.into());
                store
                    .connection
                    .execute(
                        "UPDATE native_calls SET record_json=?2 WHERE event_digest=?1",
                        params![
                            bound.event_digest,
                            String::from_utf8(canonical(&value).unwrap()).unwrap()
                        ],
                    )
                    .unwrap();
                assert!(
                    store.read_record(&bound.event_digest).is_err(),
                    "unknown native record field {pointer}"
                );
            }
            for (pointer, field) in [
                ("", "keyed"),
                ("/event", "turn"),
                ("/event", "server_source"),
            ] {
                let mut value: Value = serde_json::from_str(&original).unwrap();
                value
                    .pointer_mut(pointer)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                store
                    .connection
                    .execute(
                        "UPDATE native_calls SET record_json=?2 WHERE event_digest=?1",
                        params![
                            bound.event_digest,
                            String::from_utf8(canonical(&value).unwrap()).unwrap()
                        ],
                    )
                    .unwrap();
                assert!(
                    store.read_record(&bound.event_digest).is_err(),
                    "missing native nullable {pointer}/{field}"
                );
            }
            let mut mutations = vec![
                ("/capability", serde_json::json!([1, 2, 3])),
                ("/event/session", serde_json::json!("")),
                ("/event/server", serde_json::json!("foreign")),
                ("/context/schema", serde_json::json!("foreign")),
                ("/context/invocation_ref", serde_json::json!(Uuid::nil())),
                (
                    "/context/host_route",
                    serde_json::json!("claude-code-2.1.295"),
                ),
                (
                    "/context/state_generation",
                    serde_json::json!(bound.context.state_generation % MAX_INTEGER + 1),
                ),
                (
                    "/context/accepted_public_input_digest",
                    serde_json::json!(format!("sha256:{}", "a".repeat(64))),
                ),
                (
                    "/context/native_event_digest",
                    serde_json::json!(format!("sha256:{}", "b".repeat(64))),
                ),
                ("/context/tool_binding", serde_json::json!("other-tool")),
                ("/request/operation", serde_json::json!("unknown")),
            ];
            if bound.namespace.is_some() {
                mutations.extend([
                    (
                        "/keyed/namespace/idempotency_key",
                        serde_json::json!("other-key"),
                    ),
                    (
                        "/keyed/context/state_generation",
                        serde_json::json!(bound.context.state_generation % MAX_INTEGER + 1),
                    ),
                    (
                        "/keyed/native_session_ref",
                        serde_json::json!("other-session"),
                    ),
                    ("/keyed/native_turn_ref", serde_json::json!("other-turn")),
                    (
                        "/keyed/native_tool_use_ref",
                        serde_json::json!("other-call"),
                    ),
                    (
                        "/keyed/opaque_binding_secret_digest",
                        serde_json::json!(format!("sha256:{}", "c".repeat(64))),
                    ),
                    ("/keyed/state", serde_json::json!("accepted")),
                    ("/keyed/operation_ref", serde_json::json!(Uuid::now_v7())),
                    ("/keyed", Value::Null),
                ]);
            }
            for (pointer, value) in mutations {
                let mut changed: Value = serde_json::from_str(&original).unwrap();
                *changed.pointer_mut(pointer).unwrap() = value;
                store
                    .connection
                    .execute(
                        "UPDATE native_calls SET record_json=?2 WHERE event_digest=?1",
                        params![bound.event_digest, serde_json::to_string(&changed).unwrap()],
                    )
                    .unwrap();
                assert!(
                    store.read_record(&bound.event_digest).is_err(),
                    "stored {operation} corrupt {pointer}"
                );
                assert!(
                    store.begin_delivery(&bound).is_err(),
                    "{operation} corrupt {pointer}"
                );
            }
            store
                .connection
                .execute(
                    "UPDATE native_calls SET record_json=?2 WHERE event_digest=?1",
                    params![bound.event_digest, original],
                )
                .unwrap();
            assert!(store.begin_delivery(&bound).is_ok());
        }
    }

    #[test]
    fn checked_native_uuid_text_corpus_rejects_noncanonical_persisted_encodings() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let store = f.ready();
        let (bound, _) = store
            .bind_native(
                event("uuid-text"),
                selected_request(&store, "open_external_engagement"),
            )
            .unwrap();
        let mut context = serde_json::to_value(&bound.context).unwrap();
        let record = store.read_record(&bound.event_digest).unwrap().unwrap();
        let mut keyed = serde_json::to_value(record.keyed.as_ref().unwrap()).unwrap();
        keyed["state"] = "unresolved".into();
        let canonical = "01900000-0000-7abc-8000-000000000077";
        for raw in [
            canonical.to_owned(),
            canonical.to_uppercase(),
            canonical.replace('-', ""),
            format!("urn:uuid:{canonical}"),
            format!("{{{canonical}}}"),
            canonical.replacen("7abc", "4abc", 1),
            canonical.replacen("8000", "c000", 1),
        ] {
            context["invocation_ref"] = raw.clone().into();
            keyed["operation_ref"] = raw.clone().into();
            assert_eq!(
                serde_json::from_value::<TrustedInvocationContext>(context.clone()).is_ok(),
                raw == canonical,
                "checked invocation UUID {raw}"
            );
            assert_eq!(
                serde_json::from_value::<ProtectedInvocation>(keyed.clone()).is_ok(),
                raw == canonical,
                "checked operation UUID {raw}"
            );
        }
        keyed["operation_ref"] = Value::Null;
        assert!(serde_json::from_value::<ProtectedInvocation>(keyed).is_ok());
    }

    #[test]
    fn protected_keyed_state_corpus_keeps_receipts_only_on_acceptance() {
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let store = f.ready();
        let (bound, _) = store
            .bind_native(
                event("state-corpus"),
                selected_request(&store, "open_external_engagement"),
            )
            .unwrap();
        let record = store.read_record(&bound.event_digest).unwrap().unwrap();
        let base = record.keyed.as_ref().unwrap();
        let operation_ref = Uuid::now_v7();
        let mut receipt = OperationReceipt {
            schema: "dolgorae.operation-receipt/v1".into(),
            operation_ref: operation_ref.to_string(),
            namespace: base.namespace.clone(),
            accepted_input_digest: format!("sha256:{}", "a".repeat(64)),
            accepted_subjects: crate::delegation_identity::AcceptedSubjects {
                engagement_ref: Some(Uuid::now_v7().to_string()),
                ..Default::default()
            },
            receipt_digest: String::new(),
            accepted_revision: 0,
            result_digest: None,
        };
        receipt.receipt_digest = receipt.compute_digest().unwrap();
        for state in [
            "prepared",
            "unresolved",
            "resolved_not_accepted",
            "accepted",
            "foreign",
        ] {
            for has_ref in [false, true] {
                for has_receipt in [false, true] {
                    let mut keyed = base.clone();
                    keyed.state = state.into();
                    keyed.operation_ref = has_ref.then_some(operation_ref);
                    keyed.receipt = has_receipt.then(|| receipt.clone());
                    let valid = match state {
                        "prepared" => !has_ref && !has_receipt,
                        "unresolved" | "resolved_not_accepted" => !has_receipt,
                        "accepted" => has_ref && has_receipt,
                        _ => false,
                    };
                    assert_eq!(
                        keyed.validate().is_ok(),
                        valid,
                        "{state}/{has_ref}/{has_receipt}"
                    );
                }
            }
        }
        let mut accepted = base.clone();
        accepted.state = "accepted".into();
        accepted.operation_ref = Some(operation_ref);
        accepted.receipt = Some(receipt);
        for case in 0..3 {
            let mut bad = accepted.clone();
            let receipt = bad.receipt.as_mut().unwrap();
            match case {
                0 => receipt.receipt_digest = format!("sha256:{}", "0".repeat(64)),
                1 => receipt.namespace.idempotency_key = "other-key".into(),
                2 => receipt.operation_ref = Uuid::now_v7().to_string(),
                _ => unreachable!(),
            }
            if case != 0 {
                receipt.receipt_digest = receipt.compute_digest().unwrap();
            }
            assert!(bad.validate().is_err(), "receipt mutation {case}");
        }
        let value = serde_json::to_value(base).unwrap();
        for field in ["native_turn_ref", "operation_ref", "receipt"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<ProtectedInvocation>(missing).is_err(),
                "required nullable {field}"
            );
        }
    }

    #[test]
    fn concurrent_native_delivery_has_one_mutation_handoff_and_fresh_reads() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for operation in [
            "open_external_engagement",
            "close_external_engagement",
            "get_external_engagement",
        ] {
            let f = Fixture::new(HostRoute::Codex);
            f.complete();
            let store = f.ready();
            let request = selected_request(&store, operation);
            drop(store);
            let effects = Arc::new(AtomicUsize::new(0));
            let mut threads = Vec::new();
            for _ in 0..8 {
                let home = f.root.clone();
                let path = f.path();
                let value = request.clone();
                let effects = effects.clone();
                threads.push(std::thread::spawn(move || {
                    let home = crate::paths::DolgoraeHome::from_canonical_home(home).unwrap();
                    let store = VerifiedOwner::load(&home, &path).unwrap().ready().unwrap();
                    let (bound, _) = store.bind_native(event("concurrent-call"), value).unwrap();
                    let delivery = store.begin_delivery(&bound).unwrap();
                    if matches!(delivery, Delivery::Dispatch | Delivery::ObserveFresh) {
                        effects.fetch_add(1, Ordering::SeqCst);
                    }
                    (bound.context.invocation_ref, delivery)
                }));
            }
            let results: Vec<_> = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect();
            assert!(results.iter().all(|result| result.0 == results[0].0));
            if method_class(operation).unwrap() == MethodClass::Read {
                assert_eq!(effects.load(Ordering::SeqCst), 8);
                assert!(
                    results
                        .iter()
                        .all(|result| result.1 == Delivery::ObserveFresh)
                );
            } else {
                assert_eq!(effects.load(Ordering::SeqCst), 1);
                assert_eq!(
                    results
                        .iter()
                        .filter(|result| result.1 == Delivery::Reconcile)
                        .count(),
                    7
                );
            }
        }
    }
    #[test]
    fn consumer_witness_binds_original_owner_even_when_generations_match() {
        let a = Fixture::new(HostRoute::Codex);
        a.complete();
        let b = Fixture::new(HostRoute::Codex);
        b.complete();
        for f in [&a, &b] {
            let db = rusqlite::Connection::open(f.private.join(DATABASE)).unwrap();
            db.execute("UPDATE metadata SET generation=42", []).unwrap();
        }
        let bound_a = {
            let store = a.ready();
            store
                .bind_native(
                    event("owner-a"),
                    selected_request(&store, "open_external_engagement"),
                )
                .unwrap()
                .0
        };
        let mut bound_b = {
            let store = b.ready();
            store
                .bind_native(
                    event("owner-b"),
                    selected_request(&store, "open_external_engagement"),
                )
                .unwrap()
                .0
        };
        assert_eq!(
            bound_a.context.state_generation,
            bound_b.context.state_generation
        );
        assert!(crate::native_consumer::Witness::external(&a.owner(), &bound_a).is_ok());
        assert!(crate::native_consumer::Witness::external(&b.owner(), &bound_b).is_ok());
        assert!(crate::native_consumer::Witness::external(&b.owner(), &bound_a).is_err());
        assert!(crate::native_consumer::Witness::external(&a.owner(), &bound_b).is_err());
        bound_b.event.tool = "foreign-tool".into();
        assert!(crate::native_consumer::Witness::external(&b.owner(), &bound_b).is_err());
    }
}
