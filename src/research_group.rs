//! Immutable group admission over the existing delegation transaction owner.
use super::*;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

const CHUNK: usize = 8 * 1024 * 1024;
const KIND: &str = "research_group";
const INPUT_KIND: &str = "research_group_input";

#[cfg(test)]
thread_local! { static DISPATCH_LIMIT:std::cell::Cell<Option<usize>>=const{std::cell::Cell::new(None)}; }
#[cfg(test)]
pub(super) fn with_dispatch_limit<T>(limit: usize, call: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            DISPATCH_LIMIT.with(|cell| cell.set(None));
        }
    }
    DISPATCH_LIMIT.with(|cell| cell.set(Some(limit)));
    let _reset = Reset;
    call()
}
fn dispatch_limit() -> usize {
    #[cfg(test)]
    if let Some(limit) = DISPATCH_LIMIT.with(|cell| cell.get()) {
        return limit;
    }
    usize::MAX
}

/// A child operation of one durably accepted Controller group. Only this owner
/// constructs it after authenticating the original aggregate and retained plan.
pub(super) struct GroupOrigin {
    group: Uuid,
    index: usize,
    member: Uuid,
}
impl GroupOrigin {
    pub(super) fn value(&self) -> Value {
        json!({"schema":"dolgorae.controller-group-origin/v1","group_ref":self.group,"entry_index":self.index,"member_ref":self.member})
    }
    pub(super) fn verify(&self, connection: &rusqlite::Connection) -> Result<(), MachineError> {
        verify_origin(connection, &self.value()).map(|_| ())
    }
    pub(super) fn member(&self) -> Uuid {
        self.member
    }
    pub(super) fn consume_birth(
        &self,
        connection: &rusqlite::Connection,
    ) -> Result<(), MachineError> {
        let document = verify_origin(connection, &self.value())?;
        if document["allocations"][self.index]["new_run"] != true {
            return Err(retained_source_invalid());
        }
        let bytes = crate::storage_observation::read_content(
            connection,
            "group_member_birth",
            &self.member.to_string(),
            "reservation",
        )?
        .ok_or_else(retained_source_invalid)?;
        let birth = decode(&bytes)?;
        if birth["group_ref"] != self.group.to_string()
            || birth["member_ref"] != self.member.to_string()
        {
            return Err(retained_source_invalid());
        }
        // The member's primary key and the child's original operation key make
        // consumption single-owner. Repeated acceptance can only be its replay.
        let key = format!("group-birth-consumed:{}", self.member);
        let changed = connection
            .execute(
                "INSERT OR IGNORE INTO metadata(key,value) VALUES(?1,?2)",
                rusqlite::params![key, self.group.to_string()],
            )
            .map_err(|_| retained_source_invalid())?;
        if changed != 1 {
            return Err(retained_source_invalid());
        }
        Ok(())
    }
    pub(super) fn admit_assignment(
        &self,
        connection: &rusqlite::Connection,
        assignment: &crate::caller_scope::CallerAssignment<'_>,
    ) -> Result<(), MachineError> {
        let document = verify_origin(connection, &self.value())?;
        if document["mode"] != "controller"
            || assignment.member_ref != self.member
            || document["manifest"]["scope_kind"] != assignment.aggregate_scope_kind
            || document["manifest"]["scope_ref"] != assignment.aggregate_scope_ref
        {
            return Err(retained_source_invalid());
        }
        crate::storage_observation::put_content(
            connection,
            "controller_group_task",
            &assignment.assignment_id.to_string(),
            "origin",
            &encode(&self.value())?,
        )?;
        Ok(())
    }
    pub(super) fn verify_assignment(
        &self,
        connection: &rusqlite::Connection,
        receipt: &OperationReceipt,
    ) -> Result<(), MachineError> {
        self.verify(connection)?;
        let task = receipt
            .accepted_subjects
            .task_ref
            .as_deref()
            .ok_or_else(retained_source_invalid)?;
        let bytes = crate::storage_observation::read_content(
            connection,
            "controller_group_task",
            task,
            "origin",
        )?
        .ok_or_else(retained_source_invalid)?;
        if decode(&bytes)? != self.value() {
            return Err(retained_source_invalid());
        }
        Ok(())
    }
}
pub(crate) fn validate_managed_origin(
    connection: &rusqlite::Connection,
    origin: &Value,
    member: Uuid,
    session: Uuid,
) -> Result<(), MachineError> {
    let document = verify_origin(connection, origin)?;
    if document["mode"] != "controller"
        || document["manifest"]["scope_kind"] != "orchestrated_session"
        || document["manifest"]["scope_ref"] != session.to_string()
        || origin["member_ref"] != member.to_string()
    {
        return Err(retained_source_invalid());
    }
    Ok(())
}
fn verify_origin(connection: &rusqlite::Connection, origin: &Value) -> Result<Value, MachineError> {
    let group = uuid_field(origin, "group_ref")?;
    let index = origin["entry_index"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(retained_source_invalid)?;
    if origin["schema"] != "dolgorae.controller-group-origin/v1" {
        return Err(retained_source_invalid());
    }
    let accepted:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM delegation_operations WHERE operation_kind='assign_group' AND state='accepted' AND json_extract(accepted_subjects_json,'$.group_ref')=?1)",[group.to_string()],|r|r.get(0)).map_err(|_|retained_source_invalid())?;
    if !accepted {
        return Err(retained_source_invalid());
    }
    let bytes =
        crate::storage_observation::read_content(connection, KIND, &group.to_string(), "manifest")?
            .ok_or_else(retained_source_invalid)?;
    let document: Value = serde_json::from_slice(&bytes).map_err(|_| retained_source_invalid())?;
    if document["allocations"][index]["member_ref"] != origin["member_ref"] {
        return Err(retained_source_invalid());
    }
    Ok(document)
}
pub(crate) fn verify_accounted_task(
    connection: &rusqlite::Connection,
    task: Uuid,
) -> Result<bool, MachineError> {
    let Some(bytes) = crate::storage_observation::read_content(
        connection,
        "controller_group_task",
        &task.to_string(),
        "origin",
    )?
    else {
        return Ok(false);
    };
    let origin = decode(&bytes)?;
    let document = verify_origin(connection, &origin)?;
    if document["mode"] != "controller" {
        return Err(retained_source_invalid());
    }
    let own = owner(
        &json!({"scope_kind":document["manifest"]["scope_kind"],"scope_ref":document["manifest"]["scope_ref"]}),
    )?;
    let member = uuid_field(&origin, "member_ref")?;
    let matches:bool=match own{
        SettlementOwner::External{engagement_id}=>connection.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1 AND engagement_id=?2 AND specialist_run_id=?3)",rusqlite::params![task.to_string(),engagement_id.to_string(),member.to_string()],|r|r.get(0)),
        SettlementOwner::Managed{session_id}=>connection.query_row("SELECT EXISTS(SELECT 1 FROM brokered_tasks WHERE task_id=?1 AND session_id=?2 AND target_run_id=?3)",rusqlite::params![task.to_string(),session_id.to_string(),member.to_string()],|r|r.get(0)),
    }.map_err(|_|retained_source_invalid())?;
    if !matches {
        return Err(retained_source_invalid());
    }
    Ok(true)
}

/// Authenticate retained Controller-group work for original native-history
/// reconciliation. Absence keeps the caller on its existing native path.
pub(crate) fn authorize_external_recovery(
    state_root: &Path,
    view: &WorkspaceView,
    carrier: &crate::controller::CredentialCarrier,
    engagement_id: Uuid,
    member: Uuid,
    task: Uuid,
    manifest: &crate::run::RunManifest,
) -> Result<bool, MachineError> {
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state_root))?;
    let Some(bytes) = crate::storage_observation::read_content(
        store.connection(),
        "controller_group_task",
        &task.to_string(),
        "origin",
    )?
    else {
        if super::research_merge::verify_accounted_task(store.connection(), task)? {
            let facts = store.external_task(engagement_id, task)?;
            if facts.specialist_run_id != member
                || manifest.run_id != member
                || manifest.workspace_id != view.workspace_id
            {
                return Err(retained_source_invalid());
            }
            let (workspace, binding) = store.accepted_external_owner(engagement_id)?;
            if workspace != view.workspace_id {
                return Err(owner_refusal_for("reconcile_member_task"));
            }
            crate::controller::authorize_controller(member, "run.reconcile", &binding, carrier)?;
            return Ok(true);
        }
        return Ok(false);
    };
    let origin_value = decode(&bytes)?;
    let group = uuid_field(&origin_value, "group_ref")?;
    let index = origin_value["entry_index"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(retained_source_invalid)?;
    let document = read_document(&store, &group.to_string())?;
    if document["mode"] != "controller"
        || document["manifest"]["scope_kind"] != "external_engagement"
        || document["manifest"]["scope_ref"] != engagement_id.to_string()
        || origin_value["member_ref"] != member.to_string()
        || manifest.run_id != member
        || manifest.workspace_id != view.workspace_id
        || manifest.canonical_workspace != view.canonical_path
    {
        return Err(retained_source_invalid());
    }
    let (workspace, binding) = store.accepted_external_owner(engagement_id)?;
    if workspace != view.workspace_id || binding != manifest.controller {
        return Err(owner_refusal_for("reconcile_member_task"));
    }
    crate::controller::authorize_controller(member, "run.reconcile", &binding, carrier)?;
    let scope = DelegationScope {
        state_root,
        view,
        controller: &binding,
        trusted_invocation: None,
    };
    let owner = SettlementOwner::External { engagement_id };
    let receipt = receipt_by_key(
        &store,
        &scope,
        owner,
        "assign_external_specialist_task",
        &format!("group:{group}:{index}:assign"),
    )?
    .ok_or_else(retained_source_invalid)?;
    if receipt.accepted_subjects.member_ref.as_deref() != Some(member.to_string().as_str())
        || receipt.accepted_subjects.task_ref.as_deref() != Some(task.to_string().as_str())
    {
        return Err(retained_source_invalid());
    }
    GroupOrigin {
        group,
        index,
        member,
    }
    .verify_assignment(store.connection(), &receipt)?;
    let accepted = store
        .external_task_identity(engagement_id, task)?
        .ok_or_else(retained_source_invalid)?;
    let entry = &document["manifest"]["entries"][index];
    if entry["accepted_task_digest"] != accepted.digest()?
        || entry["role_snapshot_digest"] != accepted.role_snapshot_digest
        || entry["agent_configuration_digest"] != accepted.agent_configuration_digest
    {
        return Err(retained_source_invalid());
    }
    let (configuration, _, _) = store.external_member_configuration(engagement_id, member)?;
    if configuration != manifest.agent_configuration {
        return Err(retained_source_invalid());
    }
    store.validate_external_member_binding(
        engagement_id,
        member,
        manifest
            .aggregate_binding
            .as_ref()
            .ok_or_else(retained_source_invalid)?,
    )?;
    Ok(true)
}

/// An internal native child inherits only its accepted parent's invocation.
pub(super) fn native_recovery_parent(
    store: &EngagementStore,
    receipt: &OperationReceipt,
    accepted: &AcceptedTask,
    caller: &crate::delegation_identity::CallerScope,
) -> Result<OperationReceipt, MachineError> {
    let parts = receipt
        .namespace
        .idempotency_key
        .split(':')
        .collect::<Vec<_>>();
    let ["group", group, index, "assign"] = parts.as_slice() else {
        return Err(retained_source_invalid());
    };
    let group = Uuid::parse_str(group).map_err(|_| retained_source_invalid())?;
    let index = index
        .parse::<usize>()
        .map_err(|_| retained_source_invalid())?;
    if receipt.namespace.idempotency_key != format!("group:{group}:{index}:assign") {
        return Err(retained_source_invalid());
    }
    let document = read_document(store, &group.to_string())?;
    let entry = document["manifest"]["entries"]
        .get(index)
        .ok_or_else(retained_source_invalid)?;
    let allocation = document["allocations"]
        .get(index)
        .ok_or_else(retained_source_invalid)?;
    if document["mode"] != "native"
        || document["caller_scope"] != json!(caller)
        || document["manifest"]["scope_kind"].as_str() != receipt.namespace.scope_kind.as_deref()
        || document["manifest"]["scope_ref"].as_str() != receipt.namespace.scope_ref.as_deref()
        || allocation["member_ref"].as_str() != receipt.accepted_subjects.member_ref.as_deref()
        || entry["accepted_task_digest"] != accepted.digest()?
        || entry["role_snapshot_digest"] != accepted.role_snapshot_digest
        || entry["agent_configuration_digest"] != accepted.agent_configuration_digest
    {
        return Err(retained_source_invalid());
    }
    let parents: Vec<String> = store.connection().prepare(
        "SELECT receipt_json FROM delegation_operations WHERE operation_kind='assign_group' AND state='accepted' AND json_extract(accepted_subjects_json,'$.group_ref')=?1 LIMIT 2"
    ).map_err(|_|retained_source_invalid())?.query_map([group.to_string()],|r|r.get(0)).map_err(|_|retained_source_invalid())?.collect::<Result<_,_>>().map_err(|_|retained_source_invalid())?;
    let [parent] = parents.as_slice() else {
        return Err(retained_source_invalid());
    };
    let parent: OperationReceipt =
        serde_json::from_str(parent).map_err(|_| retained_source_invalid())?;
    if parent.namespace.workspace_ref != receipt.namespace.workspace_ref
        || parent.namespace.scope_kind != receipt.namespace.scope_kind
        || parent.namespace.scope_ref != receipt.namespace.scope_ref
        || parent.namespace.controller_principal != receipt.namespace.controller_principal
        || parent.namespace.controller_generation != receipt.namespace.controller_generation
        || parent.accepted_subjects.group_ref.as_deref() != Some(group.to_string().as_str())
    {
        return Err(retained_source_invalid());
    }
    if engagement::accepted_native_context(store.connection(), &parent)?.is_none() {
        return Err(retained_source_invalid());
    }
    Ok(parent)
}

fn invalid() -> MachineError {
    MachineError::new(
        "GROUP_MANIFEST_INVALID",
        "group input does not match its immutable manifest",
        false,
        json!({}),
    )
}
fn encode(value: &Value) -> Result<Vec<u8>, MachineError> {
    crate::operation::canonical(value).map_err(|_| invalid())
}
fn decode(bytes: &[u8]) -> Result<Value, MachineError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let parsed = crate::jcs::parse(text).map_err(|_| invalid())?;
    let canonical = crate::jcs::canonicalize(&parsed).map_err(|_| invalid())?;
    if canonical != bytes {
        return Err(invalid());
    }
    serde_json::from_slice(bytes).map_err(|_| invalid())
}
fn digest(value: &Value) -> Result<String, MachineError> {
    let mut bytes = b"dolgorae.group-assignment/v1\n".to_vec();
    bytes.extend(encode(value)?);
    Ok(format!("sha256:{}", sha256_hex(&bytes)))
}
fn uuid_field(value: &Value, field: &str) -> Result<Uuid, MachineError> {
    value[field]
        .as_str()
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(invalid)
}
pub(super) fn owner(request: &Value) -> Result<SettlementOwner, MachineError> {
    let id = uuid_field(request, "scope_ref")?;
    match request["scope_kind"].as_str() {
        Some("external_engagement") => Ok(SettlementOwner::External { engagement_id: id }),
        Some("orchestrated_session") => Ok(SettlementOwner::Managed { session_id: id }),
        _ => Err(invalid()),
    }
}

pub(super) fn authorize_owner(
    scope: &DelegationScope<'_>,
    own: SettlementOwner,
    context: Option<&ManagedCallContext<'_>>,
    controller: bool,
    operation: &'static str,
) -> Result<(), MachineError> {
    if !controller {
        return transport_owner(scope, own, context, operation);
    }
    match own {
        SettlementOwner::External { .. } => transport_owner(scope, own, None, operation),
        SettlementOwner::Managed { session_id } => {
            let broker = crate::orchestration::OrchestrationStore::open_observer(scope.state_root)?;
            let session = broker.session(session_id)?;
            managed_scope_identity(scope, &session, session_id, operation)
        }
    }
}

fn queue_owner(own: SettlementOwner) -> crate::assignment_queue::Owner {
    match own {
        SettlementOwner::External { engagement_id } => {
            crate::assignment_queue::Owner::External(engagement_id)
        }
        SettlementOwner::Managed { session_id } => {
            crate::assignment_queue::Owner::Managed(session_id)
        }
    }
}
fn allocate_members(
    store: &EngagementStore,
    own: SettlementOwner,
    captures: &[CapturedSource],
    scope: &DelegationScope<'_>,
) -> Result<Vec<Value>, MachineError> {
    let mut planned = std::collections::BTreeMap::<String, Vec<(Uuid, bool, usize, u64)>>::new();
    let mut allocations = Vec::new();
    for capture in captures {
        let snapshot =
            crate::provisioning::retained_snapshot(scope.state_root, &capture.provisioning_ref)?;
        let (_, configuration) = snapshot.admitted_configuration()?;
        let configuration_digest =
            crate::run::agent_configuration_digest(&configuration).map_err(|_| invalid())?;
        let config_key = format!("{}:{configuration_digest}", snapshot.role_name());
        let choices = planned.entry(config_key).or_default();
        let length = capture.len() as u64;
        let selected = if let Some(choice) = choices.iter_mut().find(|(_, _, count, bytes)| {
            *count < 4 && bytes.saturating_add(length) <= 256 * 1024 * 1024
        }) {
            choice.2 += 1;
            choice.3 += length;
            (choice.0, choice.1)
        } else {
            let existing = crate::assignment_queue::reusable_member(
                store.connection(),
                queue_owner(own),
                snapshot.role_name(),
                &configuration_digest,
                &snapshot.admitted.requested_access,
            )?;
            let candidate = existing
                .filter(|(id, _)| !choices.iter().any(|(used, _, _, _)| used == id))
                .and_then(|(id, _)| {
                    let available =
                        crate::group_quota::available_bytes(scope.state_root, id).ok()?;
                    let pending = member_pending(store.connection(), own, id).ok()?;
                    (available >= length && pending < 4).then_some((
                        id,
                        false,
                        pending + 1,
                        256 * 1024 * 1024 - available + length,
                    ))
                })
                .unwrap_or_else(|| (Uuid::now_v7(), true, 1, length));
            choices.push(candidate);
            (candidate.0, candidate.1)
        };
        allocations.push(json!({"member_ref":selected.0,"new_run":selected.1,"configuration_sha256":configuration_digest,"role_ref":snapshot.role_name(),"requested_access":snapshot.admitted.requested_access}));
    }
    Ok(allocations)
}
fn member_pending(
    connection: &rusqlite::Connection,
    own: SettlementOwner,
    member: Uuid,
) -> Result<usize, MachineError> {
    let query = match own {
        SettlementOwner::External { .. } => {
            "SELECT COUNT(*) FROM tasks WHERE specialist_run_id=?1 AND state IN ('accepted','queued','claimed','dispatching','running','completed_not_delivered','interrupted_unknown')"
        }
        SettlementOwner::Managed { .. } => {
            "SELECT COUNT(*) FROM brokered_tasks WHERE target_run_id=?1 AND state IN ('accepted','queued','claimed','dispatching','running','completed_not_delivered','interrupted_unknown')"
        }
    };
    let actual: usize = connection
        .query_row(query, [member.to_string()], |row| row.get(0))
        .map_err(|_| retained_source_invalid())?;
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='result_content_refs')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| retained_source_invalid())?;
    let reserved: usize = if present {
        connection.query_row("SELECT COUNT(*) FROM result_content_refs r JOIN result_content c ON c.sha256=r.sha256,
          json_each(CAST(c.bytes AS TEXT),'$.allocations') a
          WHERE r.owner_kind='research_group' AND r.representation='manifest' AND r.retired_operation IS NULL
            AND json_extract(a.value,'$.member_ref')=?1
            AND json_extract(CAST(c.bytes AS TEXT),'$.manifest.scope_kind')=?2
            AND json_extract(CAST(c.bytes AS TEXT),'$.manifest.scope_ref')=?3
            AND EXISTS(SELECT 1 FROM delegation_operations g WHERE g.operation_kind='assign_group' AND g.state='accepted' AND json_extract(g.accepted_subjects_json,'$.group_ref')=r.owner_id)
            AND NOT EXISTS(SELECT 1 FROM delegation_operations d WHERE d.scope_kind=?2 AND d.scope_ref=?3 AND d.state='accepted'
              AND d.operation_kind IN ('assign_external_specialist_task','assign_specialist_task')
              AND d.idempotency_key='group:'||r.owner_id||':'||a.key||':assign')",
          rusqlite::params![member.to_string(),own.scope_kind(),own.scope_ref().to_string()],|row|row.get(0)).map_err(|_|retained_source_invalid())?
    } else {
        0
    };
    Ok(actual.saturating_add(reserved))
}
pub(super) fn require_active_group_owner(
    connection: &rusqlite::Transaction<'_>,
    scope: &DelegationScope<'_>,
    own: SettlementOwner,
) -> Result<(), MachineError> {
    match own {
        SettlementOwner::External { engagement_id } => {
            let state: String = connection
                .query_row(
                    "SELECT state FROM engagements WHERE engagement_id=?1",
                    [engagement_id.to_string()],
                    |r| r.get(0),
                )
                .map_err(|_| retained_source_invalid())?;
            if state != "active" {
                return Err(assignment_refusal(
                    "RUN_STATE_CONFLICT",
                    "group owner is not open",
                ));
            }
        }
        SettlementOwner::Managed { session_id } => {
            require_session_still_active(connection, session_id, session_id)?
        }
    }
    let _ = scope;
    Ok(())
}
fn verify_allocations(
    connection: &rusqlite::Connection,
    own: SettlementOwner,
    allocations: &[Value],
) -> Result<(), MachineError> {
    let mut planned = std::collections::BTreeMap::<Uuid, usize>::new();
    for allocation in allocations {
        *planned
            .entry(uuid_field(allocation, "member_ref")?)
            .or_default() += 1;
    }
    for (member, count) in planned {
        if member_pending(connection, own, member)?.saturating_add(count) > 4 {
            return Err(assignment_refusal(
                "SPECIALIST_POLICY_DENIED",
                "group member queue capacity changed before acceptance",
            ));
        }
    }
    for allocation in allocations {
        if allocation["new_run"] == true {
            continue;
        }
        let member = allocation["member_ref"].as_str().ok_or_else(invalid)?;
        let (role,configuration,access,state):(String,String,String,String)=match own{
            SettlementOwner::External{engagement_id}=>connection.query_row("SELECT role_ref,configuration_sha256,requested_access,state FROM members WHERE engagement_id=?1 AND specialist_run_id=?2",rusqlite::params![engagement_id.to_string(),member],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))),
            SettlementOwner::Managed{session_id}=>connection.query_row("SELECT role_ref,agent_configuration_sha256,requested_access,membership_state FROM brokered_members WHERE session_id=?1 AND run_id=?2 AND activation_policy='successor'",rusqlite::params![session_id.to_string(),member],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))
        }.map_err(|_|retained_source_invalid())?;
        if allocation["role_ref"] != role
            || allocation["configuration_sha256"] != configuration
            || allocation["requested_access"] != access
            || state != "active"
        {
            return Err(assignment_refusal(
                "REVISION_CONFLICT",
                "selected group member changed",
            ));
        }
    }
    Ok(())
}

/// Read a regular source using no-follow traversal rooted at the caller workspace.
/// The opened inode and its size/timestamps must remain unchanged through readback.
fn open_source(root: &Path, path: &Path) -> Result<std::fs::File, MachineError> {
    let relative = path.strip_prefix(root).map_err(|_| invalid())?;
    let mut components = relative.components().peekable();
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|_| invalid())?;
    while let Some(component) = components.next() {
        let std::path::Component::Normal(name) = component else {
            return Err(invalid());
        };
        directory =
            crate::workspace::open_relative_nofollow(&directory, name, components.peek().is_some())
                .map_err(|_| invalid())?;
    }
    Ok(directory)
}
#[cfg(test)]
fn read_source(root: &Path, path: &Path, reference: &Value) -> Result<Vec<u8>, MachineError> {
    capture_source(root, path, reference).map(|(bytes, _)| bytes)
}
struct CapturedSource {
    file: std::fs::File,
    path: std::path::PathBuf,
    root: std::path::PathBuf,
    before: std::fs::Metadata,
    root_before: std::fs::Metadata,
    reference: crate::delegation_identity::BytesRef,
    provisioning_ref: String,
}
fn fingerprint(metadata: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
impl CapturedSource {
    fn len(&self) -> usize {
        self.reference.byte_length as usize
    }
    fn verify(&self) -> Result<(), MachineError> {
        let after = self.file.metadata().map_err(|_| invalid())?;
        let current = open_source(&self.root, &self.path)?
            .metadata()
            .map_err(|_| invalid())?;
        let root_after = std::fs::symlink_metadata(&self.root).map_err(|_| invalid())?;
        if fingerprint(&self.before) != fingerprint(&after)
            || fingerprint(&self.before) != fingerprint(&current)
            || (self.root_before.dev(), self.root_before.ino())
                != (root_after.dev(), root_after.ino())
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn retain(
        &self,
        connection: &rusqlite::Connection,
        group: Uuid,
        index: usize,
    ) -> Result<(), MachineError> {
        use sha2::Digest as _;
        use std::io::{Seek as _, SeekFrom};
        self.verify()?;
        let mut file = self.file.try_clone().map_err(|_| invalid())?;
        file.seek(SeekFrom::Start(0)).map_err(|_| invalid())?;
        let mut digest = sha2::Sha256::new();
        let mut remaining = self.len();
        let mut chunk = 0;
        let mut buffer = vec![0_u8; CHUNK.min(remaining)];
        while remaining > 0 {
            let length = remaining.min(CHUNK);
            file.read_exact(&mut buffer[..length])
                .map_err(|_| invalid())?;
            digest.update(&buffer[..length]);
            crate::storage_observation::put_content(
                connection,
                INPUT_KIND,
                &group.to_string(),
                &format!("{index}:{chunk}"),
                &buffer[..length],
            )?;
            chunk += 1;
            remaining -= length;
        }
        let mut extra = [0_u8; 1];
        if file.read(&mut extra).map_err(|_| invalid())? != 0
            || format!("sha256:{:x}", digest.finalize()) != self.reference.sha256
        {
            return Err(invalid());
        }
        self.verify()
    }
}
fn capture_source(
    root: &Path,
    path: &Path,
    reference: &Value,
) -> Result<(Vec<u8>, CapturedSource), MachineError> {
    let root_before = std::fs::symlink_metadata(root).map_err(|_| invalid())?;
    let mut directory = open_source(root, path)?;
    let before = directory.metadata().map_err(|_| invalid())?;
    if !before.is_file()
        || before.len() > 128 * 1024 * 1024
        || reference["byte_length"] != before.len()
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    directory
        .by_ref()
        .take(before.len() + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    let after = directory.metadata().map_err(|_| invalid())?;
    let current = open_source(root, path)?.metadata().map_err(|_| invalid())?;
    let root_after = std::fs::symlink_metadata(root).map_err(|_| invalid())?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) || (before.dev(), before.ino()) != (current.dev(), current.ino())
        || (root_before.dev(), root_before.ino()) != (root_after.dev(), root_after.ino())
        || bytes.len() as u64 != before.len()
        || reference["sha256"] != format!("sha256:{}", sha256_hex(&bytes))
    {
        return Err(invalid());
    }
    let source = CapturedSource {
        file: directory,
        path: path.to_owned(),
        root: root.to_owned(),
        before,
        root_before,
        reference: serde_json::from_value(reference.clone()).map_err(|_| invalid())?,
        provisioning_ref: String::new(),
    };
    source.verify()?;
    Ok((bytes, source))
}

fn normalized(request: &Value) -> Value {
    json!({"manifest":request["manifest"],"inputs":request["inputs"].as_array().unwrap_or(&Vec::new()).iter().map(|s|json!({"entry_key":s["entry_key"],"content":s["content"]})).collect::<Vec<_>>()})
}
pub(super) fn read_document(store: &EngagementStore, group: &str) -> Result<Value, MachineError> {
    let bytes =
        crate::storage_observation::read_content(store.connection(), KIND, group, "manifest")?
            .ok_or_else(retained_source_invalid)?;
    let document: Value = serde_json::from_slice(&bytes).map_err(|_| retained_source_invalid())?;
    let (receipt_json,envelope_json):(String,String)=store.connection().query_row(
        "SELECT receipt_json,mutation_envelope_json FROM delegation_operations WHERE operation_kind='assign_group' AND state='accepted' AND json_extract(accepted_subjects_json,'$.group_ref')=?1",
        [group],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|retained_source_invalid())?;
    let receipt: OperationReceipt =
        serde_json::from_str(&receipt_json).map_err(|_| retained_source_invalid())?;
    receipt.verify_digest()?;
    let envelope: Envelope =
        serde_json::from_str(&envelope_json).map_err(|_| retained_source_invalid())?;
    let expected =
        crate::delegation_identity::BytesRef::from_bytes(&encode(&document["normalized"])?);
    if document["mode"] == "native" {
        let caller: crate::delegation_identity::CallerScope =
            serde_json::from_value(document["caller_scope"].clone())
                .map_err(|_| retained_source_invalid())?;
        caller.validate().map_err(|_| retained_source_invalid())?;
    }
    let schema = match document["mode"].as_str() {
        Some("native") if !document["caller_scope"].is_null() => {
            "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/assign_group_normalized_body"
        }
        Some("controller") if document["caller_scope"].is_null() => {
            "https://dolgorae.local/schema/v014-identities/v1#/$defs/public_assign_group_normalized_body"
        }
        _ => return Err(retained_source_invalid()),
    };
    if receipt.namespace.operation_kind != "assign_group"
        || receipt.accepted_subjects.group_ref.as_deref() != Some(group)
        || document["manifest"]["scope_kind"] != json!(receipt.namespace.scope_kind)
        || document["manifest"]["scope_ref"] != json!(receipt.namespace.scope_ref)
        || LookupNamespace::from(&envelope.namespace) != receipt.namespace
        || envelope.request_schema != schema
        || crate::operation::envelope_digest(&envelope).map_err(|_| retained_source_invalid())?
            != receipt.accepted_input_digest
        || !matches!(engagement::intent_lookup(store.connection(), &receipt.namespace)?, IntentLookup::Accepted(ref original) if original.as_ref() == &receipt)
        || document["digest"] != digest(&document["manifest"])?
        || document["normalized"]["manifest"] != document["manifest"]
        || envelope.request_body.byte_length != expected.byte_length
        || envelope.request_body.sha256 != expected.sha256
    {
        return Err(retained_source_invalid());
    }
    Ok(document)
}
/// Group acceptance has no synthetic Task or CompleteResult. Child progress is
/// observed separately through await/collect, as in the public group lookup.
pub(super) fn lookup_assignment(
    store: &EngagementStore,
    receipt: &OperationReceipt,
) -> Result<(), MachineError> {
    let group = receipt
        .accepted_subjects
        .group_ref
        .as_deref()
        .ok_or_else(retained_source_invalid)?;
    let document = read_document(store, group)?;
    if document["manifest"]["scope_kind"] != json!(receipt.namespace.scope_kind)
        || document["manifest"]["scope_ref"] != json!(receipt.namespace.scope_ref)
    {
        return Err(retained_source_invalid());
    }
    Ok(())
}

fn receipt_by_key(
    store: &EngagementStore,
    scope: &DelegationScope<'_>,
    owner: SettlementOwner,
    operation: &str,
    key: &str,
) -> Result<Option<OperationReceipt>, MachineError> {
    match entry_lookup(store, scope, owner, operation, key)? {
        IntentLookup::Accepted(receipt) => Ok(Some(*receipt)),
        _ => Ok(None),
    }
}
fn entry_lookup(
    store: &EngagementStore,
    scope: &DelegationScope<'_>,
    owner: SettlementOwner,
    operation: &str,
    key: &str,
) -> Result<IntentLookup, MachineError> {
    let namespace = Namespace::delegation(
        &scope.controller.identity,
        &scope.view.workspace_id,
        Some((owner.scope_kind(), &owner.scope_ref().to_string())),
        operation,
        key,
    )
    .map_err(|_| invalid())?;
    engagement::intent_lookup(store.connection(), &(&namespace).into())
}

fn resume_entries(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    caller: Option<&crate::caller_scope::TrustedCallerScope>,
    port: &dyn RunSettlementPort,
    own: SettlementOwner,
    group: Uuid,
    doc: &Value,
) -> Result<(), MachineError> {
    let operation = if matches!(own, SettlementOwner::Managed { .. }) {
        "assign_specialist_task"
    } else {
        "assign_external_specialist_task"
    };
    let hire = if matches!(own, SettlementOwner::Managed { .. }) {
        "spawn_specialist"
    } else {
        "hire_external_specialist"
    };
    for (index, input) in doc["normalized"]["inputs"]
        .as_array()
        .ok_or_else(retained_source_invalid)?
        .iter()
        .enumerate()
        .take(dispatch_limit())
    {
        let store = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(
            scope.state_root,
        ))?;
        if !matches!(
            entry_lookup(
                &store,
                scope,
                own,
                operation,
                &format!("group:{group}:{index}:assign")
            )?,
            IntentLookup::Absent { .. }
        ) {
            continue;
        }
        if matches!(
            entry_lookup(
                &store,
                scope,
                own,
                hire,
                &format!(
                    "group:{group}:{}:hire",
                    doc["allocations"][index]["member_ref"]
                        .as_str()
                        .ok_or_else(retained_source_invalid)?
                )
            )?,
            IntentLookup::Unresolved { .. } | IntentLookup::NotAccepted { .. }
        ) {
            continue;
        }
        let count = doc["chunks"][index]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= 16)
            .ok_or_else(retained_source_invalid)?;
        let mut bytes = Vec::new();
        for chunk in 0..count {
            let body = crate::storage_observation::read_content(
                store.connection(),
                INPUT_KIND,
                &group.to_string(),
                &format!("{index}:{chunk}"),
            )?
            .filter(|b| b.len() <= CHUNK)
            .ok_or_else(retained_source_invalid)?;
            bytes.extend(body);
        }
        if input["content"] != json!(crate::delegation_identity::BytesRef::from_bytes(&bytes)) {
            return Err(retained_source_invalid());
        }
        let content = decode(&bytes)?;
        drop(store);
        // Original-key child admission resolves a concurrent winner, while an
        // unresolved retained child is never redispatched by this recovery.
        let outcome = dispatch_entry(
            scope,
            context,
            caller,
            port,
            own,
            group,
            index,
            &content,
            &doc["allocations"][index],
        );
        if let Err(error) = outcome {
            let mut current = open_store(scope)?;
            current.transact(|tx| {
                let representation = format!("failure:{index}");
                if crate::storage_observation::read_content(
                    tx,
                    KIND,
                    &group.to_string(),
                    &representation,
                )?
                .is_none()
                {
                    crate::storage_observation::put_content(
                        tx,
                        KIND,
                        &group.to_string(),
                        &representation,
                        &encode(&json!({"code":error.code}))?,
                    )?;
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}

/// Protected mirror read for the original native group call.
pub(super) fn native_lookup(
    scope: &DelegationScope<'_>,
    caller: &crate::caller_scope::TrustedCallerScope,
    request: &Value,
) -> Result<IntentLookup, MachineError> {
    let own = owner(request)?;
    transport_owner(scope, own, None, "lookup_operation")?;
    let proof = scope
        .trusted_invocation
        .ok_or_else(retained_source_invalid)?;
    let identity = intent_identity(
        scope,
        "assign_group",
        Some((own.scope_kind(), &own.scope_ref().to_string())),
        checked_key(request["idempotency_key"].as_str().ok_or_else(invalid)?)?,
        "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/assign_group_normalized_body",
        &normalized(request),
    )?;
    let store = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(
        scope.state_root,
    ))?;
    let snapshot = store
        .connection()
        .unchecked_transaction()
        .map_err(|_| retained_source_invalid())?;
    let observed = engagement::intent_lookup(store.connection(), &identity.lookup)?;
    if let IntentLookup::Accepted(receipt) = &observed {
        if receipt.accepted_input_digest != identity.digest {
            return Err(idempotency_conflict());
        }
        let group = receipt
            .accepted_subjects
            .group_ref
            .as_deref()
            .ok_or_else(retained_source_invalid)?;
        let document = read_document(&store, group)?;
        if document["mode"] != "native"
            || document["caller_scope"] != json!(caller.accepted())
            || engagement::accepted_native_context(store.connection(), receipt)?.as_ref()
                != Some(proof.context())
        {
            return Err(retained_source_invalid());
        }
        return Ok(observed);
    }
    if let IntentLookup::Unresolved { operation_ref, .. }
    | IntentLookup::NotAccepted { operation_ref, .. } = &observed
    {
        let invalid_context = |reason| {
            MachineError::new(
                "RECOVERY_REQUIRED",
                reason,
                false,
                json!({"subject":"delegation_operations","required_action":"restore_state","effect":"none"}),
            )
        };
        let text: Option<String> = snapshot
            .query_row(
                "SELECT trusted_invocation_json FROM delegation_operations WHERE operation_ref=?1",
                [operation_ref.to_string()],
                |row| row.get(0),
            )
            .map_err(|error| {
                MachineError::new(
                    "INTERNAL_ERROR",
                    "durable engagement authority failed",
                    false,
                    json!({"reason":error.to_string()}),
                )
            })?;
        let context: crate::delegation_identity::TrustedInvocationContext = serde_json::from_str(
            &text.ok_or_else(|| invalid_context("host invocation context is missing"))?,
        )
        .map_err(|_| invalid_context("host invocation context is malformed"))?;
        if &context != proof.context() {
            return Err(invalid_context(
                "host invocation context differs from original binding",
            ));
        }
    }
    Ok(observed)
}

/// Accept the complete capture and receipt in one transaction before any member effect.
pub(crate) fn assign(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    caller: &crate::caller_scope::TrustedCallerScope,
    port: &dyn RunSettlementPort,
    request: &Value,
) -> Result<Value, MachineError> {
    assign_in(scope, context, Some(caller), port, request).map(|(response, _)| response)
}
fn assign_in(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    caller: Option<&crate::caller_scope::TrustedCallerScope>,
    port: &dyn RunSettlementPort,
    request: &Value,
) -> Result<(Value, bool), MachineError> {
    let own = owner(request)?;
    authorize_owner(scope, own, context, caller.is_none(), "assign_group")?;
    if !crate::specialist_facade::validates(request, "assign_group_request") {
        return Err(invalid());
    }
    let key = checked_key(request["idempotency_key"].as_str().ok_or_else(invalid)?)?;
    let identity = intent_identity(
        scope,
        "assign_group",
        Some((own.scope_kind(), &own.scope_ref().to_string())),
        key,
        if caller.is_some() {
            "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/assign_group_normalized_body"
        } else {
            "https://dolgorae.local/schema/v014-identities/v1#/$defs/public_assign_group_normalized_body"
        },
        &normalized(request),
    )?;
    let mut store = open_store(scope)?;
    let snapshot = store
        .connection()
        .unchecked_transaction()
        .map_err(|_| retained_source_invalid())?;
    if let Some(receipt) = replay_outcome(
        engagement::intent_lookup(store.connection(), &identity.lookup)?,
        &identity.digest,
        "assign_group",
    )? {
        let group = receipt
            .accepted_subjects
            .group_ref
            .as_ref()
            .ok_or_else(retained_source_invalid)?;
        let doc = read_document(&store, group)?;
        if let Some(proof) = scope.trusted_invocation
            && engagement::accepted_native_context(store.connection(), &receipt)?.as_ref()
                != Some(proof.context())
        {
            return Err(retained_source_invalid());
        }
        let group_id = Uuid::parse_str(group).map_err(|_| retained_source_invalid())?;
        drop(snapshot);
        drop(store);
        if doc["mode"]
            != if caller.is_some() {
                "native"
            } else {
                "controller"
            }
            || caller.is_some_and(|c| doc["caller_scope"] != json!(c.accepted()))
        {
            return Err(caller_scope_mismatch("assign_group"));
        }
        resume_entries(scope, context, caller, port, own, group_id, &doc)?;
        return Ok((
            json!({"schema":"dolgorae.external-specialist-response/v5","operation":"assign_group","group_ref":group,"manifest_digest":doc["digest"],"receipt":receipt}),
            true,
        ));
    }
    drop(snapshot);
    let manifest = &request["manifest"];
    let entries = manifest["entries"].as_array().ok_or_else(invalid)?;
    let inputs = request["inputs"].as_array().ok_or_else(invalid)?;
    if manifest["scope_kind"] != request["scope_kind"]
        || manifest["scope_ref"] != request["scope_ref"]
        || !(1..=64).contains(&entries.len())
        || entries.len() != inputs.len()
        || encode(manifest)?.len() > 1_048_576
        || encode(&request["inputs"])?.len() > 2_097_152
    {
        return Err(invalid());
    }
    let root = scope.view.canonical_path.to_path_buf()?;
    let mut previous = None;
    let mut captures = Vec::new();
    for (entry, input) in entries.iter().zip(inputs) {
        let entry_key = entry["entry_key"].as_str().ok_or_else(invalid)?;
        if previous.is_some_and(|p: &str| p >= entry_key)
            || entry["entry_key"] != input["entry_key"]
        {
            return Err(invalid());
        }
        previous = Some(entry_key);
        let path = input["absolute_file_path"]
            .as_str()
            .filter(|p| p.len() <= 4096)
            .ok_or_else(invalid)?;
        let (bytes, mut capture) = capture_source(&root, Path::new(path), &input["content"])?;
        let content = decode(&bytes)?;
        if !crate::specialist_facade::validates(&content, "group_task_input") {
            return Err(invalid());
        }
        if store
            .accepted_install_receipt(
                &scope.view.workspace_id,
                content["provisioning_ref"].as_str().ok_or_else(invalid)?,
            )?
            .is_none()
        {
            return Err(invalid());
        }
        let snapshot = crate::provisioning::retained_snapshot(
            scope.state_root,
            content["provisioning_ref"].as_str().ok_or_else(invalid)?,
        )
        .map_err(|_| invalid())?;
        let candidate = snapshot.candidate(true).map_err(|_| invalid())?;
        let task: crate::task_request::SpecialistTaskRequest =
            serde_json::from_value(content["assignment"]["task_body"].clone())
                .map_err(|_| invalid())?;
        let accepted: AcceptedTask =
            serde_json::from_value(content["assignment"]["accepted_task_identity"].clone())
                .map_err(|_| invalid())?;
        let (_, configuration) = snapshot.admitted_configuration().map_err(|_| invalid())?;
        let (actual, _) = crate::successor_capture::capture_facade_task(
            &configuration,
            &task,
            &accepted.execution_intent,
            None,
        )
        .map_err(|_| invalid())?;
        if actual != accepted
            || entry["accepted_task_digest"] != actual.digest()?
            || entry["role_snapshot_digest"] != candidate.role_snapshot_digest
            || entry["agent_configuration_digest"] != candidate.agent_configuration_digest
        {
            return Err(invalid());
        }
        capture.provisioning_ref = content["provisioning_ref"]
            .as_str()
            .ok_or_else(invalid)?
            .to_owned();
        captures.push(capture);
    }
    let group = Uuid::now_v7();
    let allocations = allocate_members(&store, own, &captures, scope)?;
    let charges = allocations
        .iter()
        .zip(inputs)
        .map(|(a, input)| {
            Ok(crate::group_quota::InputCharge {
                entry_key: input["entry_key"].as_str().ok_or_else(invalid)?.to_owned(),
                run_id: uuid_field(a, "member_ref")?,
                content: serde_json::from_value(input["content"].clone()).map_err(|_| invalid())?,
                new_run: a["new_run"].as_bool().ok_or_else(invalid)?,
            })
        })
        .collect::<Result<Vec<_>, MachineError>>()?;
    let quota = crate::group_quota::GroupQuotaPlan::prepare(scope.state_root, group, charges)?;
    let operation = Uuid::now_v7();
    let group_digest = digest(manifest)?;
    let document = json!({"manifest":manifest,"digest":group_digest,"normalized":normalized(request),"chunks":captures.iter().map(|b|b.len().div_ceil(CHUNK)).collect::<Vec<_>>(),"allocations":allocations,"mode":if caller.is_some(){"native"}else{"controller"},"caller_scope":caller.map(|c|c.accepted())});
    let now = now_ms()?;
    let result = store.transact(|tx| {
        let preparation = engagement::intent_prepare(
            tx,
            IntentAdmission {
                operation_ref: operation,
                namespace: &identity.lookup,
                mutation_envelope_json: &identity.json,
                mutation_envelope_digest: &identity.digest,
                trusted_invocation: identity.trusted_invocation.as_ref(),
                expected_revision: request["expected_revision"].as_u64(),
                now_ms: now,
            },
        )?;
        match preparation {
            IntentPreparation::Accepted(old) => return Ok(*old),
            IntentPreparation::Unresolved { operation_ref } => {
                return Err(unresolved_refusal("assign_group", operation_ref));
            }
            IntentPreparation::Created => {}
        }
        let current=engagement::scope_membership_revision(tx,&scope.view.workspace_id,own.scope_kind(),&own.scope_ref().to_string())?;
        if request["expected_revision"].as_u64().is_some_and(|r|r!=current){return Err(assignment_refusal("REVISION_CONFLICT","group membership revision moved"));}
        require_active_group_owner(tx,scope,own)?;
        verify_allocations(tx,own,&allocations)?;
        if let Some(caller)=caller {
            let used=crate::caller_scope::rounds_used(tx,caller)?;
            if used.saturating_add(entries.len() as u64)>caller.accepted().max_rounds {
                return Err(assignment_refusal("ROUNDS_EXHAUSTED","the group exceeds the caller's remaining assignment budget"));
            }
        }
        quota.commit_in(tx)?;
        crate::storage_observation::put_content(
            tx,
            KIND,
            &group.to_string(),
            "manifest",
            &encode(&document)?,
        )?;
        for (index, source) in captures.iter().enumerate() {
            source.retain(tx,group,index)?;
        }
        for allocation in &allocations { if allocation["new_run"]==true {
            crate::storage_observation::put_content(tx,"group_member_birth",allocation["member_ref"].as_str().ok_or_else(invalid)?,"reservation",&encode(&json!({"schema":"dolgorae.group-member-birth/v1","group_ref":group,"member_ref":allocation["member_ref"]}))?)?;
        }}
        let receipt=build_receipt(&identity.namespace,operation,&identity.digest,AcceptedSubjects{group_ref:Some(group.to_string()),..Default::default()},current)?;
        engagement::intent_accept(tx, operation, &receipt, now)?;
        Ok(receipt)
    })?;
    drop(quota);
    drop(captures); // Durable SQL captures are the only remaining input consumer.
    let group = result
        .accepted_subjects
        .group_ref
        .clone()
        .ok_or_else(retained_source_invalid)?;
    let exact_replay = result.operation_ref != operation.to_string();
    {
        let store = open_store(scope)?;
        let document = read_document(&store, &group)?;
        if document["mode"]
            != if caller.is_some() {
                "native"
            } else {
                "controller"
            }
            || caller.is_some_and(|c| document["caller_scope"] != json!(c.accepted()))
        {
            return Err(caller_scope_mismatch("assign_group"));
        }
        drop(store);
        resume_entries(
            scope,
            context,
            caller,
            port,
            own,
            Uuid::parse_str(&group).map_err(|_| retained_source_invalid())?,
            &document,
        )?;
    }
    Ok((
        json!({"schema":"dolgorae.external-specialist-response/v5","operation":"assign_group","group_ref":group,"manifest_digest":group_digest,"receipt":result}),
        exact_replay,
    ))
}

pub(crate) fn public_owner_binding(
    state_root: &Path,
    workspace_id: &str,
    carrier: &crate::controller::CredentialCarrier,
    scope_kind: &str,
    scope_ref: Uuid,
    root_primary: Option<Uuid>,
) -> Result<crate::run::ControllerBinding, MachineError> {
    let binding = match scope_kind {
        "external_engagement" => {
            if root_primary.is_some() {
                return Err(owner_refusal_for("group"));
            }
            let store = EngagementStore::open_read_only(
                &EngagementStore::workspace_database_path(state_root),
            )?;
            let (workspace, binding) = store.external_owner_binding(scope_ref)?;
            if workspace != workspace_id {
                return Err(owner_refusal_for("group"));
            }
            binding
        }
        "orchestrated_session" => {
            let observer = crate::orchestration::OrchestrationStore::open_observer(state_root)?;
            let session = observer.session(scope_ref)?;
            if session.workspace_id != workspace_id
                || root_primary.is_some_and(|root| root != session.root_run_id)
            {
                return Err(owner_refusal_for("group"));
            }
            crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, state_root)
                .load_controller_binding(session.root_run_id)?
        }
        _ => return Err(invalid()),
    };
    crate::controller::authorize_controller(scope_ref, "group", &binding, carrier)?;
    Ok(binding)
}

/// Controller-authenticated public admission and observation. Public callers do
/// not acquire native tool provenance or a Podway Action accounting scope.
#[allow(clippy::too_many_arguments)]
pub(crate) fn public_call(
    home: &crate::paths::DolgoraeHome,
    view: &WorkspaceView,
    carrier: &crate::controller::CredentialCarrier,
    root_primary: Option<Uuid>,
    scope_kind: &str,
    scope_ref: Uuid,
    operation: &str,
    key: Option<&str>,
    expected_revision: Option<u64>,
    request: &Value,
    port: &dyn RunSettlementPort,
) -> Result<Value, MachineError> {
    let state_root = home.workspace_root(&view.workspace_id);
    let binding = public_owner_binding(
        &state_root,
        &view.workspace_id,
        carrier,
        scope_kind,
        scope_ref,
        root_primary,
    )?;
    let scope = DelegationScope {
        view,
        state_root: &state_root,
        controller: &binding,
        trusted_invocation: None,
    };
    if request["scope_kind"] != scope_kind
        || request["scope_ref"] != scope_ref.to_string()
        || request["operation"] != operation
        || (matches!(
            operation,
            "assign_group" | "merge_group_results" | "assign_synthesis"
        ) && (request["idempotency_key"].as_str() != key
            || request["expected_revision"].as_u64() != expected_revision))
    {
        return Err(invalid());
    }
    let own = owner(request)?;
    let execute = || -> Result<Value, MachineError> {
        if operation == "assign_synthesis" {
            let exact_replay = super::research_merge::accepted_key(
                &scope,
                operation,
                request["idempotency_key"].as_str().ok_or_else(invalid)?,
                own,
            )?;
            let response = super::research_merge::assign(&scope, None, None, port, request, true)?;
            let member = Uuid::parse_str(
                response["task"]["member_ref"]
                    .as_str()
                    .ok_or_else(retained_source_invalid)?,
            )
            .map_err(|_| retained_source_invalid())?;
            let task = Uuid::parse_str(
                response["task"]["task_ref"]
                    .as_str()
                    .ok_or_else(retained_source_invalid)?,
            )
            .map_err(|_| retained_source_invalid())?;
            let collected = collect_group_task(&scope, None, port, own, member, task)?;
            let store = open_store(&scope)?;
            let accepted = match own {
                SettlementOwner::External { engagement_id } => store
                    .external_task_identity(engagement_id, task)?
                    .ok_or_else(retained_source_invalid)?,
                SettlementOwner::Managed { session_id } => {
                    crate::successor_capture::retained_accepted_task(
                        &crate::orchestration::successor_task(
                            store.connection(),
                            session_id,
                            task,
                        )?
                        .ok_or_else(retained_source_invalid)?
                        .accepted_request_json,
                    )?
                }
            };
            let manifest =
                crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, &state_root)
                    .load_manifest(member)?;
            let delegation = manifest
                .instructions
                .delegation()
                .ok_or_else(retained_source_invalid)?;
            let revision = engagement::scope_membership_revision(
                store.connection(),
                &view.workspace_id,
                own.scope_kind(),
                &own.scope_ref().to_string(),
            )?;
            let rounds:u64=store.connection().query_row("SELECT COUNT(*) FROM delegation_operations WHERE operation_kind='assign_synthesis' AND state='accepted' AND json_extract(accepted_subjects_json,'$.task_ref')=?1",[task.to_string()],|row|row.get(0)).map_err(|_|retained_source_invalid())?;
            let result = collected
                .complete_result
                .as_ref()
                .map(|value| {
                    let result = serde_json::from_value(value.clone())
                        .map_err(|_| retained_source_invalid())?;
                    crate::result_component_artifact::public_result(&state_root, view, &result)
                })
                .transpose()?;
            return Ok(
                json!({"response":response,"accepted_task":accepted,"source_revision":revision,"rounds":rounds,"disposition":collected.disposition.state,"role_body":delegation.role_snapshot.role_body,"role_snapshot_digest":delegation.role_snapshot_digest,"instruction_identity_digest":delegation.instruction_identity_digest,"controller_kind":binding.identity.kind,"exact_replay":exact_replay,"results":result.into_iter().collect::<Vec<_>>() }),
            );
        }
        if operation == "merge_group_results" {
            let mut request = request.clone();
            if request["group_assignment_digest"].is_null() {
                let store = open_store(&scope)?;
                request["group_assignment_digest"] = read_document(
                    &store,
                    request["group_ref"].as_str().ok_or_else(invalid)?,
                )?["digest"]
                    .clone();
            }
            let exact_replay = super::research_merge::accepted_key(
                &scope,
                "merge_group_results",
                request["idempotency_key"].as_str().ok_or_else(invalid)?,
                own,
            )?;
            let response = super::research_merge::merge(&scope, None, port, &request, true)?;
            let merge = Uuid::parse_str(
                response["receipt"]["accepted_subjects"]["merge_ref"]
                    .as_str()
                    .ok_or_else(retained_source_invalid)?,
            )
            .map_err(|_| retained_source_invalid())?;
            let publication = super::research_merge::public_merged(&scope, merge)?;
            return Ok(
                json!({"response":response,"merge_id":publication["merge_id"],"merged_output":publication["merged_output"],"controller_kind":binding.identity.kind,"exact_replay":exact_replay}),
            );
        }
        let (mut response, exact_replay) = match operation {
            "assign_group" => assign_in(&scope, None, None, port, request)?,
            "await_group" => (await_in(&scope, None, port, request, true)?, false),
            "collect_group" => (collect_in(&scope, None, port, request, true)?, false),
            _ => return Err(invalid()),
        };
        let group = response["group_ref"]
            .as_str()
            .or_else(|| response["coverage"]["group_ref"].as_str())
            .ok_or_else(retained_source_invalid)?
            .to_owned();
        let store = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(
            &state_root,
        ))?;
        let document = read_document(&store, &group)?;
        let source_revision = engagement::scope_membership_revision(
            store.connection(),
            &view.workspace_id,
            scope_kind,
            &scope_ref.to_string(),
        )?;
        drop(store);
        if operation == "assign_group" {
            let coverage = collect_in(
                &scope,
                None,
                port,
                &json!({"scope_kind":scope_kind,"scope_ref":scope_ref,"group_ref":group}),
                true,
            )?;
            response["coverage"] = coverage["coverage"].clone();
        }
        let (results, source_manifest) = if operation == "collect_group" {
            let offset = request["cursor"]
                .as_str()
                .and_then(|s| s.rsplit_once(':'))
                .and_then(|(_, n)| n.parse::<usize>().ok())
                .unwrap_or(0);
            public_results(
                &scope,
                own,
                port,
                &mut response["coverage"],
                document["manifest"]["entries"]
                    .as_array()
                    .ok_or_else(retained_source_invalid)?
                    .len(),
                offset,
                encode(&document["manifest"])?.len(),
            )?
        } else {
            (Vec::new(), None)
        };
        Ok(
            json!({"response":response,"manifest":document["manifest"],"source_revision":source_revision,"controller_kind":binding.identity.kind,"exact_replay":exact_replay,"results":results,"source_manifest":source_manifest}),
        )
    };
    execute().map_err(|mut error| {
        if let Some(key) = key
            && let Ok(value) = public_lookup(&state_root, &view.workspace_id, carrier, scope_kind, scope_ref, operation, key) {
                let mut details = error.details.as_object().cloned().unwrap_or_default();
                details.insert("public_v2_operation".into(), json!({"controller_kind":binding.identity.kind,"principal_ref":binding.identity.controller_id,"controller_generation":binding.identity.generation,"workspace_id":view.workspace_id,"scope_kind":scope_kind,"scope_ref":scope_ref,"idempotency_key":key}));
                details.insert("public_v2_accepted".into(), json!(value["state"] == "accepted"));
                if value["state"] == "accepted" { for subject in ["group_ref", "merge_ref", "task_ref"] { details.insert(subject.into(),value["accepted_subjects"][subject].clone()); } }
                if value["state"] == "unresolved" { details.insert("effect".into(),json!("unresolved")); }
                error.details = Value::Object(details);
        }
        error
    })
}

#[allow(clippy::too_many_arguments)]
fn public_results(
    scope: &DelegationScope<'_>,
    own: SettlementOwner,
    port: &dyn RunSettlementPort,
    coverage: &mut Value,
    total: usize,
    offset: usize,
    fixed_bytes: usize,
) -> Result<(Vec<Value>, Option<Value>), MachineError> {
    let mut results = Vec::new();
    let mut consumed = 0;
    let mut bytes = fixed_bytes.saturating_add(encode(coverage)?.len());
    let mut source_manifest = None;
    let sources = coverage["sources"]
        .as_array()
        .ok_or_else(retained_source_invalid)?
        .clone();
    for source in &sources {
        if source["result_digest"].is_null() {
            consumed += 1;
            continue;
        }
        let member = uuid_field(source, "worker_ref")?;
        let task = uuid_field(source, "task_ref")?;
        let collected = collect_group_task(scope, None, port, own, member, task)?;
        let result: crate::delegation_identity::CompleteResult = serde_json::from_value(
            collected
                .complete_result
                .ok_or_else(retained_source_invalid)?,
        )
        .map_err(|_| retained_source_invalid())?;
        if source["result_digest"] != result.digest()? {
            return Err(assignment_refusal(
                "REVISION_CONFLICT",
                "group result changed during collection",
            ));
        }
        let row =
            crate::result_component_artifact::public_result(scope.state_root, scope.view, &result)?;
        let size = encode(&row)?.len();
        if bytes.saturating_add(size) > 3 * 1024 * 1024 {
            if consumed == 0 {
                if !row["source_manifest"].is_object() {
                    return Err(retained_source_invalid());
                }
                source_manifest = Some(row["source_manifest"].clone());
                consumed = 1;
            }
            break;
        }
        bytes += size;
        results.push(row);
        consumed += 1;
    }
    coverage["sources"]
        .as_array_mut()
        .ok_or_else(retained_source_invalid)?
        .truncate(consumed);
    let end = offset + consumed;
    coverage["complete"] = json!(end == total);
    coverage["next_cursor"] = if end < total {
        json!(format!(
            "{}:{end}",
            coverage["group_ref"]
                .as_str()
                .ok_or_else(retained_source_invalid)?
        ))
    } else {
        Value::Null
    };
    Ok((results, source_manifest))
}

pub(super) fn collect_group_task(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    port: &dyn RunSettlementPort,
    own: SettlementOwner,
    member: Uuid,
    task: Uuid,
) -> Result<CollectedTask, MachineError> {
    match own {
        SettlementOwner::External { engagement_id } => collect_external_specialist_task(
            scope,
            port,
            CollectExternalSpecialistTask {
                engagement_id,
                member_ref: member,
                task_ref: task,
                adapter_id: "dolgorae.group/1",
                adapter_version: "1",
            },
        ),
        SettlementOwner::Managed { .. } => {
            if let Some(context) = context {
                return collect_specialist_task(
                    scope,
                    context,
                    port,
                    CollectSpecialistTask {
                        member_ref: member,
                        task_ref: task,
                        adapter_id: "dolgorae.group/1",
                        adapter_version: "1",
                    },
                );
            }
            // Read the authenticated aggregate directly. A Controller reader
            // has no native invocation context to manufacture or replay.
            authorize_owner(scope, own, None, true, "collect_group")?;
            let store = EngagementStore::open_read_only(
                &EngagementStore::workspace_database_path(scope.state_root),
            )?;
            observe_collected_task(scope, port, &store, own, member, task)
                .map(|(collected, _)| collected)
        }
    }
}

/// Retained original-key recovery is authenticated before revealing whether a
/// group key exists. It reads no caller files and creates no runtime state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn public_lookup(
    state_root: &Path,
    workspace_id: &str,
    carrier: &crate::controller::CredentialCarrier,
    scope_kind: &str,
    scope_ref: Uuid,
    operation: &str,
    key: &str,
) -> Result<Value, MachineError> {
    if !matches!(
        operation,
        "assign_group" | "merge_group_results" | "assign_synthesis"
    ) {
        return Err(invalid());
    }
    let binding = public_owner_binding(
        state_root,
        workspace_id,
        carrier,
        scope_kind,
        scope_ref,
        None,
    )?;
    let namespace = Namespace::delegation(
        &binding.identity,
        workspace_id,
        Some((scope_kind, &scope_ref.to_string())),
        operation,
        checked_key(key)?,
    )
    .map_err(|reason| MachineError::invalid_argument("namespace", reason))?;
    engagement::require_delegation_namespace(state_root)?;
    let lookup: LookupNamespace = (&namespace).into();
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state_root))?;
    match engagement::intent_lookup(store.connection(), &lookup)? {
        IntentLookup::Accepted(receipt) => {
            if operation == "merge_group_results" {
                let merge = Uuid::parse_str(
                    receipt
                        .accepted_subjects
                        .merge_ref
                        .as_deref()
                        .ok_or_else(retained_source_invalid)?,
                )
                .map_err(|_| retained_source_invalid())?;
                let publication = super::research_merge::record(&store, merge)?;
                if publication["workspace_id"] != workspace_id
                    || publication["scope_kind"] != scope_kind
                    || publication["scope_ref"] != scope_ref.to_string()
                {
                    return Err(owner_refusal_for("lookup_operation"));
                }
                return Ok(
                    json!({"schema":LOOKUP_SCHEMA,"namespace":lookup,"state":"accepted","operation_ref":receipt.operation_ref,"accepted_input_digest":receipt.accepted_input_digest,"accepted_subjects":receipt.accepted_subjects,"receipt":receipt,"current_outcome":"completed","result_ref":merge,"result_digest":publication["digest"]}),
                );
            }
            if operation == "assign_synthesis" {
                let task = Uuid::parse_str(
                    receipt
                        .accepted_subjects
                        .task_ref
                        .as_deref()
                        .ok_or_else(retained_source_invalid)?,
                )
                .map_err(|_| retained_source_invalid())?;
                let binding = super::research_merge::task_binding(store.connection(), task)?
                    .ok_or_else(retained_source_invalid)?;
                if binding["workspace_id"] != workspace_id
                    || binding["scope_kind"] != scope_kind
                    || binding["scope_ref"] != scope_ref.to_string()
                    || binding["controller"] != true
                {
                    return Err(owner_refusal_for("lookup_operation"));
                }
                let member = uuid_field(&binding, "member_ref")?;
                let own = owner(&binding)?;
                let state: String = store
                    .connection()
                    .query_row(
                        if scope_kind == "external_engagement" {
                            "SELECT state FROM tasks WHERE task_id=?1"
                        } else {
                            "SELECT state FROM brokered_tasks WHERE task_id=?1"
                        },
                        [task.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(|_| retained_source_invalid())?;
                let accepted: AcceptedTask = serde_json::from_value(
                    binding["normalized"]["assignment"]["accepted_task_identity"].clone(),
                )
                .map_err(|_| retained_source_invalid())?;
                let result = complete_result_of(
                    &store,
                    own,
                    member,
                    task,
                    &accepted.digest()?,
                    &state,
                    operation,
                )?;
                let publication = result
                    .as_ref()
                    .map(|(digest, _, result)| {
                        crate::result_component_artifact::retained_source_identity(
                            store.connection(),
                            &task.to_string(),
                            digest,
                            Some(result),
                        )
                        .map(|(_, publication)| publication)
                    })
                    .transpose()?;
                return Ok(
                    json!({"schema":LOOKUP_SCHEMA,"namespace":lookup,"state":"accepted","operation_ref":receipt.operation_ref,"accepted_input_digest":receipt.accepted_input_digest,"accepted_subjects":receipt.accepted_subjects,"receipt":receipt,"current_outcome":result.as_ref().map(|(_,_,result)|result.terminal_outcome.as_str()).unwrap_or("pending"),"result_ref":publication.as_ref().map(|publication|publication["result_id"].clone()),"result_digest":result.as_ref().map(|(_,_,result)|result.digest()).transpose()?}),
                );
            }
            let group = receipt
                .accepted_subjects
                .group_ref
                .as_deref()
                .ok_or_else(retained_source_invalid)?;
            let document = read_document(&store, group)?;
            if document["mode"] != "controller" {
                return Err(owner_refusal_for("lookup_operation"));
            }
            Ok(
                json!({"schema":LOOKUP_SCHEMA,"namespace":lookup,"state":"accepted","operation_ref":receipt.operation_ref,"accepted_input_digest":receipt.accepted_input_digest,"accepted_subjects":receipt.accepted_subjects,"receipt":receipt,"current_outcome":"pending","result_ref":null,"result_digest":null}),
            )
        }
        IntentLookup::NotAccepted {
            operation_ref,
            input_digest,
            refusal_code,
            durable_refusal_revision,
        } => Ok(
            json!({"schema":LOOKUP_SCHEMA,"namespace":lookup,"state":"not_accepted","operation_ref":operation_ref,"input_digest":input_digest,"refusal_code":refusal_code,"durable_refusal_revision":durable_refusal_revision,"exact_key_retry_permitted":true}),
        ),
        IntentLookup::Unresolved {
            operation_ref,
            known_subjects,
        } => Ok(
            json!({"schema":LOOKUP_SCHEMA,"namespace":lookup,"state":"unresolved","operation_ref":operation_ref,"known_subjects":known_subjects,"effect":"unresolved","required_action":"reconcile_operation"}),
        ),
        IntentLookup::Absent { durable_watermark } => Ok(
            json!({"schema":LOOKUP_SCHEMA,"namespace":lookup,"state":"absent","proof":{"authoritative":true,"complete":true,"retained_namespace_generation":namespace.controller_generation,"durable_watermark":durable_watermark,"tombstones_checked":true,"matching_operation":false,"matching_tombstone":false}}),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_entry(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    caller: Option<&crate::caller_scope::TrustedCallerScope>,
    port: &dyn RunSettlementPort,
    own: SettlementOwner,
    group: Uuid,
    index: usize,
    input: &Value,
    allocation: &Value,
) -> Result<(), MachineError> {
    let member = uuid_field(allocation, "member_ref")?;
    let origin = GroupOrigin {
        group,
        index,
        member,
    };
    origin.verify(open_store(scope)?.connection())?;
    let local = DelegationScope {
        view: scope.view,
        state_root: scope.state_root,
        controller: scope.controller,
        trusted_invocation: None,
    };
    let snapshot = crate::provisioning::retained_snapshot(
        scope.state_root,
        input["provisioning_ref"].as_str().ok_or_else(invalid)?,
    )?;
    let candidate = snapshot.candidate(true)?;
    if allocation["new_run"] == true {
        let hire_key = format!("group:{group}:{member}:hire");
        match own {
            SettlementOwner::External { engagement_id } => {
                hire_external_specialist_in(
                    &local,
                    port,
                    HireExternalSpecialist {
                        engagement_id,
                        provisioning_ref: &candidate.provisioning_ref,
                        role_ref: snapshot.role_name(),
                        agent_configuration_digest: &candidate.agent_configuration_digest,
                        idempotency_key: &hire_key,
                        expected_revision: None,
                    },
                    Some(&origin),
                )?;
            }
            SettlementOwner::Managed { session_id } => {
                let public_context = ManagedCallContext {
                    session_id,
                    source_run_id: session_id,
                    source_turn_id: "",
                    source_tool_call_id: "",
                    idempotency_key: &hire_key,
                };
                let ctx = if let Some(native) = context {
                    ManagedCallContext {
                        idempotency_key: &hire_key,
                        ..*native
                    }
                } else {
                    public_context
                };
                spawn_specialist_in(
                    &local,
                    &ctx,
                    port,
                    SpawnSpecialist {
                        provisioning_ref: &candidate.provisioning_ref,
                        role_ref: snapshot.role_name(),
                        agent_configuration_digest: &candidate.agent_configuration_digest,
                        expected_revision: None,
                    },
                    Some(&origin),
                )?;
            }
        }
    }
    let task =
        serde_json::from_value(input["assignment"]["task_body"].clone()).map_err(|_| invalid())?;
    let accepted: AcceptedTask =
        serde_json::from_value(input["assignment"]["accepted_task_identity"].clone())
            .map_err(|_| invalid())?;
    let deadline = input["assignment"]["deadline_seconds"]
        .as_u64()
        .ok_or_else(invalid)?;
    let key = format!("group:{group}:{index}:assign");
    let accounting = caller.map_or(
        AssignmentAccounting::Group(&origin),
        AssignmentAccounting::Native,
    );
    match own {
        SettlementOwner::External { engagement_id } => {
            assign_external_specialist_task_in(
                &local,
                accounting,
                port,
                AssignExternalSpecialistTask {
                    engagement_id,
                    member_ref: member,
                    task,
                    execution_intent: &accepted.execution_intent,
                    target: None,
                    deadline_seconds: deadline,
                    idempotency_key: &key,
                    expected_revision: None,
                },
                None,
                Some(1),
            )?;
        }
        SettlementOwner::Managed { session_id } => {
            let public_context = ManagedCallContext {
                session_id,
                source_run_id: session_id,
                source_turn_id: "",
                source_tool_call_id: "",
                idempotency_key: &key,
            };
            let ctx = if let Some(native) = context {
                ManagedCallContext {
                    idempotency_key: &key,
                    ..*native
                }
            } else {
                public_context
            };
            assign_specialist_task_in(
                &local,
                &ctx,
                accounting,
                port,
                AssignSpecialistTask {
                    member_ref: member,
                    task,
                    execution_intent: &accepted.execution_intent,
                    target: None,
                    deadline_seconds: deadline,
                    expected_revision: None,
                },
                None,
                Some(1),
            )?;
        }
    }
    Ok(())
}

pub(crate) fn collect(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    port: &dyn RunSettlementPort,
    request: &Value,
) -> Result<Value, MachineError> {
    collect_in(scope, context, port, request, false)
}
pub(super) fn collect_in(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    port: &dyn RunSettlementPort,
    request: &Value,
    controller: bool,
) -> Result<Value, MachineError> {
    let own = owner(request)?;
    authorize_owner(scope, own, context, controller, "collect_group")?;
    let group = uuid_field(request, "group_ref")?;
    let store = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(
        scope.state_root,
    ))?;
    let doc = read_document(&store, &group.to_string())?;
    if doc["manifest"]["scope_kind"] != own.scope_kind()
        || doc["manifest"]["scope_ref"] != own.scope_ref().to_string()
    {
        return Err(owner_refusal_for("collect_group"));
    }
    let entries = doc["manifest"]["entries"]
        .as_array()
        .ok_or_else(retained_source_invalid)?;
    let limit = request["limit"].as_u64().unwrap_or(64);
    if !(1..=64).contains(&limit) {
        return Err(invalid());
    }
    let prefix = format!("{group}:");
    let offset = match request["cursor"].as_str() {
        None => 0,
        Some(cursor) => cursor
            .strip_prefix(&prefix)
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n < entries.len())
            .ok_or_else(invalid)?,
    };
    let mut sources = Vec::new();
    for (index, entry) in entries.iter().enumerate().skip(offset).take(limit as usize) {
        let operation = if matches!(own, SettlementOwner::Managed { .. }) {
            "assign_specialist_task"
        } else {
            "assign_external_specialist_task"
        };
        let key = format!("group:{group}:{index}:assign");
        let receipt = receipt_by_key(&store, scope, own, operation, &key)?;
        let mut row = json!({"entry_key":entry["entry_key"],"worker_ref":null,"task_ref":null,"role_snapshot_digest":entry["role_snapshot_digest"],"agent_configuration_digest":entry["agent_configuration_digest"],"accepted_task_digest":entry["accepted_task_digest"],"state":"missing","result_digest":null,"safe_failure_code":null});
        if let Some(receipt) = receipt {
            let member = receipt
                .accepted_subjects
                .member_ref
                .as_deref()
                .ok_or_else(retained_source_invalid)?;
            let task = receipt
                .accepted_subjects
                .task_ref
                .as_deref()
                .ok_or_else(retained_source_invalid)?;
            row["worker_ref"] = json!(member);
            row["task_ref"] = json!(task);
            let member = Uuid::parse_str(member).map_err(|_| retained_source_invalid())?;
            let task = Uuid::parse_str(task).map_err(|_| retained_source_invalid())?;
            let observation = collect_group_task(scope, context, port, own, member, task);
            match observation {
                Ok(value) => {
                    let terminal = matches!(
                        value.task.state.as_str(),
                        "completed" | "failed" | "cancelled" | "expired"
                    );
                    row["state"] = json!(if terminal {
                        value.task.state.as_str()
                    } else {
                        "unknown"
                    });
                    row["result_digest"] = if terminal {
                        json!(value.task.result_digest)
                    } else {
                        Value::Null
                    };
                    if terminal {
                        row["safe_failure_code"] = value
                            .complete_result
                            .as_ref()
                            .map(|result| result["safe_failure_code"].clone())
                            .unwrap_or(Value::Null);
                    }
                }
                Err(error) => {
                    row["state"] = json!("unknown");
                    row["safe_failure_code"] = json!(error.code);
                }
            }
        } else {
            if let IntentLookup::Unresolved { known_subjects, .. } =
                entry_lookup(&store, scope, own, operation, &key)?
            {
                row["worker_ref"] = json!(known_subjects.member_ref);
                row["task_ref"] = json!(known_subjects.task_ref);
                row["state"] = json!("unknown");
            }
            if row["worker_ref"].is_null() {
                let hire = if matches!(own, SettlementOwner::Managed { .. }) {
                    "spawn_specialist"
                } else {
                    "hire_external_specialist"
                };
                let member = doc["allocations"][index]["member_ref"]
                    .as_str()
                    .ok_or_else(retained_source_invalid)?;
                match entry_lookup(
                    &store,
                    scope,
                    own,
                    hire,
                    &format!("group:{group}:{member}:hire"),
                )? {
                    IntentLookup::Accepted(receipt) => {
                        row["worker_ref"] = json!(receipt.accepted_subjects.member_ref)
                    }
                    IntentLookup::Unresolved { known_subjects, .. } => {
                        row["worker_ref"] = json!(known_subjects.member_ref);
                        row["state"] = json!("unknown");
                    }
                    _ => {}
                }
            }
            if let Some(bytes) = crate::storage_observation::read_content(
                store.connection(),
                KIND,
                &group.to_string(),
                &format!("failure:{index}"),
            )? {
                let failure: Value =
                    serde_json::from_slice(&bytes).map_err(|_| retained_source_invalid())?;
                row["safe_failure_code"] = failure["code"].clone();
            }
        }
        sources.push(row);
    }
    let end = (offset + limit as usize).min(entries.len());
    let complete = end == entries.len();
    let next = if complete {
        None
    } else {
        Some(format!("{prefix}{end}"))
    };
    Ok(
        json!({"schema":"dolgorae.external-specialist-response/v5","operation":"collect_group","coverage":{"schema":"dolgorae.group-coverage/v1","group_ref":group,"group_assignment_digest":doc["digest"],"sources":sources,"complete":complete,"next_cursor":next}}),
    )
}

pub(crate) fn await_group(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    port: &dyn RunSettlementPort,
    request: &Value,
) -> Result<Value, MachineError> {
    await_in(scope, context, port, request, false)
}
fn await_in(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    port: &dyn RunSettlementPort,
    request: &Value,
    controller: bool,
) -> Result<Value, MachineError> {
    let wait = if controller {
        Duration::from_millis(
            request["timeout_ms"]
                .as_u64()
                .filter(|n| *n <= 3_600_000)
                .ok_or_else(invalid)?,
        )
    } else {
        Duration::from_secs(
            request["timeout_seconds"]
                .as_u64()
                .filter(|n| *n <= 3600)
                .ok_or_else(invalid)?,
        )
    };
    let deadline = std::time::Instant::now() + wait;
    let mut observation = request.clone();
    observation["cursor"] = Value::Null;
    observation["limit"] = json!(64);
    loop {
        let epoch = crate::storage_observation::epoch()?;
        let mut result = collect_in(scope, context, port, &observation, controller)?;
        let sources = result["coverage"]["sources"]
            .as_array()
            .ok_or_else(retained_source_invalid)?;
        let k = match request["predicate"]["kind"].as_str() {
            Some("any") => 1,
            Some("all") => sources.len(),
            Some("k") => request["predicate"]["k"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0 && *n <= sources.len())
                .ok_or_else(invalid)?,
            _ => return Err(invalid()),
        };
        let terminal = sources
            .iter()
            .filter(|r| {
                matches!(
                    r["state"].as_str(),
                    Some("completed" | "failed" | "cancelled" | "expired")
                )
            })
            .count();
        let satisfied = terminal >= k;
        let now = std::time::Instant::now();
        if satisfied || now >= deadline {
            result["operation"] = json!("await_group");
            result["predicate_satisfied"] = json!(satisfied);
            result["wait_timed_out"] = json!(!satisfied);
            return Ok(result);
        }
        crate::storage_observation::wait(epoch, deadline.saturating_duration_since(now))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_capture_streams_bounded_chunks_and_rolls_back_source_races() {
        let root = Path::new("/tmp").join(format!("dolgorae-group-stream-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let bytes = encode(&json!({"body":"a".repeat(CHUNK+17)})).unwrap();
        let path = root.join("input.json");
        std::fs::write(&path, &bytes).unwrap();
        let (_, source) = capture_source(
            &root,
            &path,
            &json!(crate::delegation_identity::BytesRef::from_bytes(&bytes)),
        )
        .unwrap();
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE result_content(sha256 TEXT PRIMARY KEY,bytes BLOB NOT NULL); CREATE TABLE result_content_refs(owner_kind TEXT,owner_id TEXT,representation TEXT,sha256 TEXT,byte_length INTEGER,retired_operation TEXT,PRIMARY KEY(owner_kind,owner_id,representation));").unwrap();
        let group = Uuid::now_v7();
        let tx = connection.transaction().unwrap();
        source.retain(&tx, group, 0).unwrap();
        tx.commit().unwrap();
        let maximum: u64 = connection
            .query_row(
                "SELECT MAX(byte_length) FROM result_content_refs",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(maximum, CHUNK as u64);
        let mut restored = Vec::new();
        for chunk in 0..2 {
            restored.extend(
                crate::storage_observation::read_content(
                    &connection,
                    INPUT_KIND,
                    &group.to_string(),
                    &format!("0:{chunk}"),
                )
                .unwrap()
                .unwrap(),
            );
        }
        assert_eq!(restored, bytes);
        let other = root.join("other.json");
        std::fs::write(&other, b"{}").unwrap();
        let (_, changed) = capture_source(
            &root,
            &other,
            &json!(crate::delegation_identity::BytesRef::from_bytes(b"{}")),
        )
        .unwrap();
        std::fs::write(&other, b"[]").unwrap();
        let rejected = Uuid::now_v7();
        let tx = connection.transaction().unwrap();
        source.retain(&tx, rejected, 0).unwrap();
        assert_eq!(
            changed.retain(&tx, rejected, 1).unwrap_err().code,
            "GROUP_MANIFEST_INVALID"
        );
        drop(tx);
        let retained: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM result_content_refs WHERE owner_id=?1",
                [rejected.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            retained, 0,
            "a later source race rolls back earlier captured chunks"
        );
        drop((source, changed));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn group_source_reads_reject_aliases_escape_and_changed_bytes() {
        let root = std::env::temp_dir().join(format!("dolgorae-group-source-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("input.json");
        let bytes = br#"{"schema":"test"}"#;
        std::fs::write(&path, bytes).unwrap();
        let reference = json!(crate::delegation_identity::BytesRef::from_bytes(bytes));
        assert_eq!(read_source(&root, &path, &reference).unwrap(), bytes);
        let alias = root.join("alias.json");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert_eq!(
            read_source(&root, &alias, &reference).unwrap_err().code,
            "GROUP_MANIFEST_INVALID"
        );
        assert_eq!(
            read_source(&root, &root.join("../outside.json"), &reference)
                .unwrap_err()
                .code,
            "GROUP_MANIFEST_INVALID"
        );
        std::fs::write(&path, br#"{"schema":"fail"}"#).unwrap();
        assert_eq!(
            read_source(&root, &path, &reference).unwrap_err().code,
            "GROUP_MANIFEST_INVALID"
        );
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(128 * 1024 * 1024 + 1).unwrap();
        assert_eq!(
            read_source(&root, &path, &json!({"byte_length":128*1024*1024+1}))
                .unwrap_err()
                .code,
            "GROUP_MANIFEST_INVALID"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn group_capture_requires_exact_jcs_and_preserves_identity_domains() {
        assert!(decode(br#"{"duplicate":1,"duplicate":2}"#).is_err());
        assert!(decode(br#"{ "a":1}"#).is_err());
        let manifest = json!({"schema":"dolgorae.group-assignment/v1","scope_kind":"external_engagement","scope_ref":"scope","entries":[]});
        let mut normalized = manifest.clone();
        normalized["scope_kind"] = json!("orchestrated_session");
        assert_ne!(digest(&manifest).unwrap(), digest(&normalized).unwrap());
        let mut request = json!({"manifest":manifest,"inputs":[{"entry_key":"one","absolute_file_path":"/a","content":{"byte_length":1,"sha256":"same"}}]});
        let original = normalized_body_for_test(&request);
        request["inputs"][0]["absolute_file_path"] = json!("/b");
        assert_eq!(original, normalized_body_for_test(&request));
        request["inputs"][0]["content"]["byte_length"] = json!(2);
        assert_ne!(original, normalized_body_for_test(&request));
    }

    fn normalized_body_for_test(value: &Value) -> Value {
        normalized(value)
    }
}
