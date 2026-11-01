//! Confidential checked orchestration materialization, never a public observer.
use super::{
    OrchestrationBarrier, OrchestrationStore, checked, checked_deadline_ms, digest, digest_value,
    integrity, internal, parse_uuid, successor_member, table_present, validate_event_chain_on,
};
use crate::darwin::DarwinSystem;
use crate::domain::ControllerKind;
use crate::jcs::sha256_hex;
use crate::machine::MachineError;
use crate::run::ControllerBinding;
use crate::session_policy::{SessionPolicySnapshot, SessionRoleSnapshot};
use crate::trusted_host::PrivateDirectory;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use rusqlite::{OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum CapturedRole {
    Policy(SessionRoleSnapshot),
    Delegated(crate::delegation_identity::RoleSnapshot),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateController {
    controller_id: Uuid,
    generation: u64,
    kind: ControllerKind,
    principal_sha256: String,
    capability_sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u32,
    workspace_id: String,
    revision: u64,
    head_sequence: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    head_hash: Option<String>,
    aggregate_operations: Vec<AggregateOperation>,
    orchestrated_sessions: Vec<Session>,
    external_engagements: Vec<External>,
    spawn_operations: Vec<SpawnOperation>,
    specialist_tasks: Vec<Task>,
    activation_operations: Vec<Activation>,
    collaboration_exchanges: Vec<Exchange>,
    mailbox_items: Vec<Mailbox>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateOperation {
    operation_id: Uuid,
    operation_kind: String,
    aggregate_kind: String,
    aggregate_id: Uuid,
    idempotency_key: String,
    request_sha256: String,
    #[serde(deserialize_with = "Option::deserialize")]
    root_run_id: Option<Uuid>,
    #[serde(deserialize_with = "Option::deserialize")]
    external_controller_ref: Option<Value>,
    #[serde(deserialize_with = "Option::deserialize")]
    controller_binding: Option<AggregateController>,
    state: String,
    #[serde(deserialize_with = "Option::deserialize")]
    safe_error_code: Option<String>,
    requested_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    provisioning_started_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    ready_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    finished_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    aggregate_kind: String,
    semantic_owner: String,
    session_id: Uuid,
    bootstrap_operation_id: Uuid,
    root_run_id: Uuid,
    status: String,
    composition_state: String,
    approval_policy: String,
    specialist_policy: SessionPolicySnapshot,
    specialist_policy_sha256: String,
    #[serde(deserialize_with = "Option::deserialize")]
    collaboration_policy_sha256: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    collaboration_policy: Option<Value>,
    revision: u64,
    members: Vec<Member>,
    created_at: String,
    updated_at: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    run_id: Uuid,
    parent_run_id: Uuid,
    origin: String,
    runtime_owner: String,
    semantic_owner: String,
    role_ref: String,
    role_snapshot_sha256: String,
    role_snapshot: CapturedRole,
    agent_configuration_sha256: String,
    spawn_operation_id: Uuid,
    #[serde(deserialize_with = "Option::deserialize")]
    spawned_by_turn_id: Option<String>,
    membership_state: String,
    actor_residency: String,
    activation_policy: String,
    #[serde(deserialize_with = "Option::deserialize")]
    current_activation_operation_id: Option<Uuid>,
    joined_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    retired_at: Option<String>,
    agent_configuration: crate::run::AgentConfigurationSnapshot,
    controller_binding: ControllerBinding,
    requested_access: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct External {
    aggregate_kind: String,
    semantic_owner: String,
    engagement_id: Uuid,
    bootstrap_operation_id: Uuid,
    external_controller_ref: Value,
    controller_binding: AggregateController,
    #[serde(deserialize_with = "Option::deserialize")]
    label: Option<String>,
    status: String,
    revision: u64,
    members: Vec<ExternalMember>,
    created_at: String,
    updated_at: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalMember {
    run_id: Uuid,
    origin: String,
    runtime_owner: String,
    semantic_owner: String,
    role_ref: String,
    role_snapshot_sha256: String,
    agent_configuration_sha256: String,
    hire_operation_id: Uuid,
    membership_state: String,
    actor_residency: String,
    activation_policy: String,
    #[serde(deserialize_with = "Option::deserialize")]
    current_activation_operation_id: Option<Uuid>,
    joined_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    released_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnOperation {
    operation_id: Uuid,
    aggregate_kind: String,
    aggregate_id: Uuid,
    idempotency_key: String,
    #[serde(deserialize_with = "Option::deserialize")]
    parent_run_id: Option<Uuid>,
    #[serde(deserialize_with = "Option::deserialize")]
    child_run_id: Option<Uuid>,
    #[serde(deserialize_with = "Option::deserialize")]
    approval_request_id: Option<Uuid>,
    requested_spec_sha256: String,
    state: String,
    #[serde(deserialize_with = "Option::deserialize")]
    safe_error_code: Option<String>,
    requested_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    provisioning_started_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    ready_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    finished_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    task_id: Uuid,
    task_kind: String,
    aggregate_id: Uuid,
    idempotency_key: String,
    #[serde(deserialize_with = "Option::deserialize")]
    source_run_id: Option<Uuid>,
    #[serde(deserialize_with = "Option::deserialize")]
    source_turn_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    external_request_ref: Option<Value>,
    target_run_id: Uuid,
    #[serde(deserialize_with = "Option::deserialize")]
    target_turn_id: Option<String>,
    request_artifact_ref: Uuid,
    #[serde(deserialize_with = "Option::deserialize")]
    result_artifact_ref: Option<Uuid>,
    priority_class: String,
    state: String,
    created_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    safe_error_code: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    deadline_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    claim_lease_until: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    claim_owner: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    claimed_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    queued_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    dispatched_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    completed_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    delivered_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    activation_id: Uuid,
    aggregate_kind: String,
    aggregate_id: Uuid,
    run_id: Uuid,
    trigger_kind: String,
    state: String,
    attempt: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    lease_owner: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    lease_until: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    safe_error_code: Option<String>,
    requested_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    started_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    completed_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Exchange {
    exchange_id: Uuid,
    session_id: Uuid,
    root_task_id: Uuid,
    #[serde(deserialize_with = "Option::deserialize")]
    parent_exchange_id: Option<Uuid>,
    idempotency_key: String,
    source_run_id: Uuid,
    source_turn_id: String,
    source_tool_call_id: String,
    target_run_id: Uuid,
    #[serde(deserialize_with = "Option::deserialize")]
    target_turn_id: Option<String>,
    request_artifact_ref: Uuid,
    #[serde(deserialize_with = "Option::deserialize")]
    context_manifest_ref: Option<Uuid>,
    #[serde(deserialize_with = "Option::deserialize")]
    result_artifact_ref: Option<Uuid>,
    priority_class: String,
    blocks_source_turn: bool,
    hop_depth: u64,
    enqueue_sequence: u64,
    execution_state: String,
    delivery_state: String,
    #[serde(deserialize_with = "Option::deserialize")]
    safe_error_code: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    claim_owner: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    claim_lease_until: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    deadline_at: Option<String>,
    created_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    claimed_at: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    dispatched_at: Option<String>,
    completed_at: Option<String>,
    acknowledged_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mailbox {
    aggregate_kind: String,
    aggregate_id: Uuid,
    mailbox_run_id: Uuid,
    mailbox_sequence: u64,
    item_kind: String,
    related_id: Uuid,
    priority_class: String,
    blocks_source_turn: bool,
    item_state: String,
    lease_owner: Option<String>,
    lease_until: Option<String>,
    created_at: String,
    delivered_at: Option<String>,
    acknowledged_at: Option<String>,
}

#[derive(Serialize)]
struct HeadBasis {
    domain: &'static str,
    workspace_id: String,
    orchestration_events: ManagedHeads,
    events: ExternalHead,
}
#[derive(Serialize)]
struct ManagedHeads {
    sequence: u64,
    session_heads: Vec<(Uuid, u64, String)>,
}
#[derive(Serialize)]
struct ExternalHead {
    sequence: u64,
    event_hash: Option<String>,
}

fn rows<T>(
    c: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, MachineError> {
    let mut statement = c.prepare(sql).map_err(internal)?;
    statement
        .query_map(parameters, map)
        .map_err(internal)?
        .collect::<Result<_, _>>()
        .map_err(internal)
}

fn head(c: &Connection, workspace: &str) -> Result<(u64, Option<String>), MachineError> {
    let mut managed = ManagedHeads {
        sequence: 0,
        session_heads: Vec::new(),
    };
    if table_present(c, "orchestration_events")? {
        for session in rows(
            c,
            "SELECT session_id FROM orchestrated_sessions ORDER BY session_id",
            [],
            |r| r.get::<_, String>(0),
        )? {
            let session = parse_uuid(&session)?;
            validate_event_chain_on(c, session)?;
            let (sequence, hash): (u64,String) = c.query_row(
                "SELECT sequence,event_hash FROM orchestration_events WHERE session_id=?1 ORDER BY sequence DESC LIMIT 1",
                [session.to_string()],|r|Ok((r.get(0)?,r.get(1)?))
            ).map_err(internal)?;
            managed.sequence = managed.sequence.max(sequence);
            managed.session_heads.push((session, sequence, hash));
        }
    }
    let mut external = ExternalHead {
        sequence: 0,
        event_hash: None,
    };
    if table_present(c, "events")? {
        let mut previous = "0".repeat(64);
        for (sequence, aggregate, kind, payload, old, hash) in rows(
            c,
            "SELECT sequence,engagement_id,kind,payload_sha256,previous_hash,event_hash FROM events ORDER BY sequence",
            [],
            |r| {
                Ok((
                    r.get::<_, u64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )? {
            let aggregate = parse_uuid(&aggregate)?;
            digest(&payload, "event payload")?;
            let expected =
                sha256_hex(format!("{previous}\0{aggregate}\0{kind}\0{payload}").as_bytes());
            if old != previous || hash != expected || sequence <= external.sequence {
                return Err(integrity("materialization event chain is invalid"));
            }
            previous = hash.clone();
            external = ExternalHead {
                sequence,
                event_hash: Some(hash),
            };
        }
    }
    let sequence = managed
        .sequence
        .checked_add(external.sequence)
        .ok_or_else(|| integrity("materialization head overflows"))?;
    if sequence == 0 {
        return Ok((0, None));
    }
    let basis = HeadBasis {
        domain: "orchestration-state-v2-head",
        workspace_id: workspace.into(),
        orchestration_events: managed,
        events: external,
    };
    Ok((sequence, Some(digest_value(&basis)?)))
}

fn timestamp(ms: i64) -> Result<String, MachineError> {
    let ms = u64::try_from(ms).map_err(|_| integrity("materialization timestamp is invalid"))?;
    let instant = UNIX_EPOCH
        .checked_add(Duration::from_millis(ms))
        .ok_or_else(|| integrity("materialization timestamp overflows"))?;
    Ok(crate::ledger::format_system_timestamp(instant))
}

fn aggregate_controller(
    binding: ControllerBinding,
    workspace: &str,
) -> Result<AggregateController, MachineError> {
    let ns = crate::operation::Namespace::delegation(
        &binding.identity,
        workspace,
        None,
        "create_orchestrated_session",
        "checked-state",
    )
    .map_err(internal)?;
    Ok(AggregateController {
        controller_id: binding.identity.controller_id,
        generation: binding.identity.generation,
        kind: binding.identity.kind,
        principal_sha256: ns
            .controller_principal
            .strip_prefix("sha256:")
            .ok_or_else(|| integrity("controller principal is invalid"))?
            .into(),
        capability_sha256: binding.capability_sha256,
    })
}

fn current_activation(c: &Connection, run: Uuid) -> Result<Option<Uuid>, MachineError> {
    if !table_present(c, "scheduler_actors")? {
        return Ok(None);
    }
    let id: Option<String> = c.query_row(
        "SELECT operation_id FROM scheduler_actors WHERE target_run_id=?1 AND operation_kind='activation'",
        [run.to_string()], |r|r.get(0)
    ).optional().map_err(internal)?.flatten();
    id.as_deref().map(parse_uuid).transpose()
}

fn retained_policy(
    c: &Connection,
    session: Uuid,
) -> Result<(Option<Value>, Option<String>), MachineError> {
    if !table_present(c, "collaboration_policies")? {
        return Ok((None, None));
    }
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT policy_json,policy_sha256 FROM collaboration_policies WHERE session_id=?1",
            [session.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(internal)?;
    if let Some((text, digest)) = row {
        if sha256_hex(text.as_bytes()) != digest {
            return Err(integrity("captured policy digest is invalid"));
        }
        Ok((
            Some(serde_json::from_str(&text).map_err(internal)?),
            Some(digest),
        ))
    } else {
        let has_effect: bool = if table_present(c, "collaboration_exchanges")? {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM collaboration_exchanges WHERE session_id=?1)",
                [session.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)?
        } else {
            false
        };
        if has_effect {
            return Err(integrity("collaboration effects have no retained policy"));
        }
        Ok((None, None))
    }
}

fn terminal_state(state: &str) -> bool {
    matches!(state, "completed" | "aborted")
}

/// The selected basis includes durable wait identities and edge rows, even
/// when a stale or expired wait has no current blocking flag in the export.
fn wait_basis(c: &Connection) -> Result<Value, MachineError> {
    if !table_present(c, "collaboration_waits")? {
        return Ok(Value::Null);
    }
    let waits = rows(
        c,
        "SELECT wait_id,session_id,source_run_id,source_thread_id,source_turn_id,source_generation,call_id,request_sha256,expires_at_ms,active FROM collaboration_waits ORDER BY wait_id",
        [],
        |r| {
            Ok(serde_json::json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, u64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, bool>(9)?
            ]))
        },
    )?;
    let edges = rows(
        c,
        "SELECT wait_id,exchange_id,target_run_id FROM collaboration_wait_edges ORDER BY wait_id,exchange_id",
        [],
        |r| {
            Ok(serde_json::json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            ]))
        },
    )?;
    Ok(serde_json::json!({"waits":waits,"edges":edges}))
}

#[derive(Clone, Eq, PartialEq)]
struct OutputIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    digest: String,
    head_sequence: u64,
}
fn output_identity(
    directory: &PrivateDirectory,
    workspace: &str,
) -> Result<Option<OutputIdentity>, MachineError> {
    let mut file =
        match DarwinSystem.openat_nofollow(&directory.file, OsStr::new("state.json"), false) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(integrity("materialization destination is unsafe")),
        };
    let before = file.metadata().map_err(internal)?;
    if !before.is_file()
        || before.uid() != DarwinSystem.current_uid()
        || before.mode() & 0o777 != 0o600
        || before.nlink() != 1
    {
        return Err(integrity("materialization destination is unsafe"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(before.len().saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(internal)?;
    if bytes.len() as u64 != before.len() {
        return Err(integrity("materialization destination moved"));
    }
    let after = file.metadata().map_err(internal)?;
    if (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) != (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) {
        return Err(integrity("materialization destination moved"));
    }
    let old: State = serde_json::from_slice(&bytes)
        .map_err(|_| integrity("materialization destination is unrecognized"))?;
    if old.schema_version != 2
        || old.workspace_id != workspace
        || old.revision != old.head_sequence
        || (old.head_sequence == 0) != old.head_hash.is_none()
    {
        return Err(integrity("materialization destination is foreign"));
    }
    validate(
        &old,
        directory
            .path
            .parent()
            .ok_or_else(|| integrity("materialization root is absent"))?,
    )?;
    let current = DarwinSystem
        .statat_nofollow(&directory.file, OsStr::new("state.json"))
        .map_err(internal)?;
    if current.device != before.dev() || current.inode != before.ino() {
        return Err(integrity("materialization destination moved"));
    }
    Ok(Some(OutputIdentity {
        device: before.dev(),
        inode: before.ino(),
        length: before.len(),
        modified: (before.mtime(), before.mtime_nsec()),
        changed: (before.ctime(), before.ctime_nsec()),
        digest: sha256_hex(&bytes),
        head_sequence: old.head_sequence,
    }))
}

/// Explicit owner refresh. Preparation and fsync happen without a write
/// transaction; publication rechecks the captured authority under one fence.
pub(super) fn materialize(
    store: &mut OrchestrationStore,
    workspace: &str,
) -> Result<(), MachineError> {
    store.connection.check_identity()?;
    let directory = PrivateDirectory::open(&store.state_root.join("orchestration"))?;
    let previous = output_identity(&directory, workspace)?;
    let stamp = store.connection.observation_stamp()?;
    let files = capture_files(store)?;
    let (state, basis) = {
        let tx = store.connection.unchecked_transaction().map_err(internal)?;
        let state = build(store, workspace)?;
        let basis = selected_basis(&tx, workspace)?;
        store
            .faults
            .check(OrchestrationBarrier::AfterCheckedStatePreparation)?;
        if store.connection.observation_stamp()? != stamp || capture_files(store)? != files {
            return Err(integrity("materialization source moved"));
        }
        (state, basis)
    };
    if previous
        .as_ref()
        .is_some_and(|old| old.head_sequence > state.head_sequence)
    {
        return Err(integrity("materialization would regress"));
    }
    let bytes = serde_json::to_vec(&state).map_err(internal)?;
    let name = format!(".state-{}.pending", Uuid::now_v7());
    let mut pending = directory.create_file(&name)?;
    let pending_meta = pending.metadata().map_err(internal)?;
    let result = (|| {
        pending.write_all(&bytes).map_err(internal)?;
        pending.sync_all().map_err(internal)?;
        let ready = pending.metadata().map_err(internal)?;
        store
            .faults
            .check(OrchestrationBarrier::BeforeCheckedStateFence)?;
        directory.verify()?;
        store.connection.check_identity()?;
        if store.connection.observation_stamp()? != stamp {
            return Err(integrity("materialization source moved"));
        }
        let tx = Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate)
            .map_err(internal)?;
        // Complete content/capture validation was prepared outside this fence.
        // The checked DB stamp catches all commits, including same-head blob
        // changes; selected scalar metadata and file identities bind the image.
        if selected_basis(&tx, workspace)? != basis || capture_files(store)? != files {
            return Err(integrity("materialization source moved"));
        }
        verify_output_unchanged(&directory, previous.as_ref())?;
        directory.verify()?;
        store.connection.check_identity()?;
        if store.connection.observation_stamp()? != stamp || capture_files(store)? != files {
            return Err(integrity("materialization source moved"));
        }
        let fresh = pending.metadata().map_err(internal)?;
        let named = DarwinSystem
            .statat_nofollow(&directory.file, OsStr::new(&name))
            .map_err(internal)?;
        if fresh.dev() != pending_meta.dev()
            || fresh.ino() != pending_meta.ino()
            || fresh.nlink() != 1
            || fresh.mode() & 0o777 != 0o600
            || fresh.uid() != DarwinSystem.current_uid()
            || fresh.len() != bytes.len() as u64
            || (
                fresh.mtime(),
                fresh.mtime_nsec(),
                fresh.ctime(),
                fresh.ctime_nsec(),
            ) != (
                ready.mtime(),
                ready.mtime_nsec(),
                ready.ctime(),
                ready.ctime_nsec(),
            )
            || named.device != fresh.dev()
            || named.inode != fresh.ino()
        {
            return Err(integrity("materialization pending inode moved"));
        }
        if previous.is_some() {
            DarwinSystem
                .renameat_file(&directory.file, OsStr::new(&name), OsStr::new("state.json"))
                .map_err(internal)?;
        } else {
            DarwinSystem
                .renameat_exclusive(&directory.file, OsStr::new(&name), OsStr::new("state.json"))
                .map_err(internal)?;
        }
        directory.file.sync_all().map_err(internal)?;
        // No SQL writes: ending the fence cannot publish invented domain state.
        tx.rollback().map_err(internal)?;
        Ok(())
    })();
    // Only the freshly owned pending inode may be removed on a failed attempt.
    if result.is_err()
        && directory.verify().is_ok()
        && DarwinSystem
            .statat_nofollow(&directory.file, OsStr::new(&name))
            .is_ok_and(|m| m.device == pending_meta.dev() && m.inode == pending_meta.ino())
    {
        let _ = DarwinSystem.unlinkat_file(&directory.file, OsStr::new(&name));
    }
    result
}

fn build(store: &OrchestrationStore, workspace: &str) -> Result<State, MachineError> {
    let c = &*store.connection;
    let (sequence, hash) = head(c, workspace)?;
    let mut state = State {
        schema_version: 2,
        workspace_id: workspace.into(),
        revision: sequence,
        head_sequence: sequence,
        head_hash: hash,
        aggregate_operations: Vec::new(),
        orchestrated_sessions: Vec::new(),
        external_engagements: Vec::new(),
        spawn_operations: Vec::new(),
        specialist_tasks: Vec::new(),
        activation_operations: Vec::new(),
        collaboration_exchanges: Vec::new(),
        mailbox_items: Vec::new(),
    };
    sessions(store, &mut state)?;
    externals(store, &mut state)?;
    managed_spawns(c, &mut state)?;
    tasks(store, &mut state)?;
    exchanges(store, &mut state)?;
    activations(c, &mut state)?;
    validate(&state, &store.state_root)?;
    Ok(state)
}

fn sessions(store: &OrchestrationStore, state: &mut State) -> Result<(), MachineError> {
    let c = &*store.connection;
    if !table_present(c, "orchestrated_sessions")? {
        return Ok(());
    }
    for (id, created, updated) in rows(
        c,
        "SELECT session_id,created_at_ms,updated_at_ms FROM orchestrated_sessions WHERE workspace_id=?1 ORDER BY session_id",
        [&state.workspace_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    )? {
        let id = parse_uuid(&id)?;
        let accepted = store.session(id)?;
        let (operation,key,request,phase,error,op_created,op_updated): (String,String,String,String,Option<String>,i64,i64)=c.query_row(
            "SELECT operation_id,idempotency_key,request_sha256,state,safe_error_code,created_at_ms,updated_at_ms FROM aggregate_bootstrap_operations WHERE session_id=?1",
            [id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))
        ).map_err(internal)?;
        state.aggregate_operations.push(AggregateOperation {
            operation_id: parse_uuid(&operation)?,
            operation_kind: "create_orchestrated_session".into(),
            aggregate_kind: "orchestrated_session".into(),
            aggregate_id: id,
            idempotency_key: key,
            request_sha256: request,
            root_run_id: Some(id),
            external_controller_ref: None,
            controller_binding: None,
            state: phase.clone(),
            safe_error_code: error,
            requested_at: timestamp(op_created)?,
            provisioning_started_at: None,
            ready_at: (phase == "ready")
                .then(|| timestamp(op_updated))
                .transpose()?,
            finished_at: matches!(phase.as_str(), "ready" | "failed" | "cancelled")
                .then(|| timestamp(op_updated))
                .transpose()?,
        });
        let (policy, policy_hash) = retained_policy(c, id)?;
        let mut members = Vec::new();
        for (run, joined, changed, marker) in rows(
            c,
            "SELECT run_id,created_at_ms,updated_at_ms,activation_policy FROM brokered_members WHERE session_id=?1 ORDER BY run_id",
            [id.to_string()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )? {
            let run = parse_uuid(&run)?;
            let member = successor_member(c, id, run)?
                .ok_or_else(|| integrity("materialized member is absent"))?;
            let (role_hash,config_hash,controller,parent,source_turn): (String,String,String,String,Option<String>)=c.query_row(
                "SELECT role_snapshot_sha256,agent_configuration_sha256,controller_binding_json,parent_run_id,spawned_by_turn_id FROM brokered_members WHERE run_id=?1 AND session_id=?2",
                params![run.to_string(),id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))
            ).map_err(internal)?;
            let configuration = member.agent_configuration;
            let captured_role = if marker == "successor" {
                if let Some(instructions) = configuration.instructions.delegation() {
                    if instructions
                        .role_snapshot
                        .digest()?
                        .trim_start_matches("sha256:")
                        != role_hash
                    {
                        return Err(integrity("materialized delegated Role digest differs"));
                    }
                    CapturedRole::Delegated(instructions.role_snapshot.clone())
                } else {
                    // Historical successor rows may carry an older checked
                    // policy configuration. Preserve that original alternative.
                    let role = accepted.specialist_policy.role(&member.role_ref)?;
                    if role.agent_configuration() != &configuration
                        || digest_value(&role)? != role_hash
                    {
                        return Err(integrity("materialized historical successor Role differs"));
                    }
                    CapturedRole::Policy(role)
                }
            } else {
                let role = accepted.specialist_policy.role(&member.role_ref)?;
                if role.agent_configuration() != &configuration || digest_value(&role)? != role_hash
                {
                    return Err(integrity("materialized policy Role differs"));
                }
                CapturedRole::Policy(role)
            };
            let retired = member.membership_state == "retired";
            members.push(Member {
                run_id: run,
                parent_run_id: parse_uuid(&parent)?,
                origin: "brokered".into(),
                runtime_owner: "dolgorae".into(),
                semantic_owner: "dolgorae".into(),
                role_ref: member.role_ref,
                role_snapshot_sha256: role_hash,
                role_snapshot: captured_role,
                agent_configuration_sha256: config_hash,
                spawn_operation_id: member.spawn_operation_id,
                spawned_by_turn_id: source_turn,
                membership_state: member.membership_state,
                actor_residency: member.actor_residency,
                activation_policy: marker,
                current_activation_operation_id: current_activation(c, run)?,
                joined_at: timestamp(joined)?,
                retired_at: retired.then(|| timestamp(changed)).transpose()?,
                agent_configuration: configuration,
                controller_binding: serde_json::from_str(&controller).map_err(internal)?,
                requested_access: c
                    .query_row(
                        "SELECT requested_access FROM brokered_members WHERE run_id=?1",
                        [run.to_string()],
                        |r| r.get(0),
                    )
                    .map_err(internal)?,
            });
        }
        let composition = if members.iter().any(|m| m.membership_state != "retired") {
            "brokered_hierarchy"
        } else {
            "standalone_primary"
        };
        state.orchestrated_sessions.push(Session {
            aggregate_kind: "orchestrated_session".into(),
            semantic_owner: "dolgorae".into(),
            session_id: id,
            bootstrap_operation_id: accepted.bootstrap_operation_id,
            root_run_id: accepted.root_run_id,
            status: accepted.status,
            composition_state: composition.into(),
            approval_policy: accepted.approval_policy,
            specialist_policy: accepted.specialist_policy,
            specialist_policy_sha256: accepted.specialist_policy_sha256,
            collaboration_policy: policy,
            collaboration_policy_sha256: policy_hash,
            revision: accepted.revision,
            members,
            created_at: timestamp(created)?,
            updated_at: timestamp(updated)?,
        });
    }
    Ok(())
}

fn managed_spawns(c: &Connection, state: &mut State) -> Result<(), MachineError> {
    if !table_present(c, "brokered_spawn_operations")? {
        return Ok(());
    }
    for session in &state.orchestrated_sessions {
        for (operation, key, parent, child, approval, digest, phase, error, created, updated) in
            rows(
                c,
                "SELECT operation_id,idempotency_key,parent_run_id,child_run_id,approval_request_id,request_sha256,state,safe_error_code,created_at_ms,updated_at_ms FROM brokered_spawn_operations WHERE session_id=?1 ORDER BY operation_id",
                [session.session_id.to_string()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, i64>(9)?,
                    ))
                },
            )?
        {
            let phase = match phase.as_str() {
                "approved" | "publication_pending" => "provisioning",
                other => other,
            }
            .to_owned();
            state.spawn_operations.push(SpawnOperation {
                operation_id: parse_uuid(&operation)?,
                aggregate_kind: "orchestrated_session".into(),
                aggregate_id: session.session_id,
                idempotency_key: key,
                parent_run_id: Some(parse_uuid(&parent)?),
                child_run_id: child.as_deref().map(parse_uuid).transpose()?,
                approval_request_id: approval.as_deref().map(parse_uuid).transpose()?,
                requested_spec_sha256: digest,
                state: phase.clone(),
                safe_error_code: error,
                requested_at: timestamp(created)?,
                provisioning_started_at: None,
                ready_at: (phase == "ready").then(|| timestamp(updated)).transpose()?,
                finished_at: matches!(phase.as_str(), "ready" | "denied" | "failed" | "cancelled")
                    .then(|| timestamp(updated))
                    .transpose()?,
            });
        }
    }
    Ok(())
}

fn external_operation(
    c: &Connection,
    workspace: &str,
    kind: &str,
    subject: &str,
    id: Uuid,
) -> Result<(String, String), MachineError> {
    if table_present(c, "delegation_operations")? {
        let query = format!(
            "SELECT receipt_json FROM delegation_operations WHERE workspace_ref=?1 AND operation_kind=?2 AND state='accepted' AND json_extract(accepted_subjects_json,'$.{subject}')=?3"
        );
        let receipts = rows(c, &query, params![workspace, kind, id.to_string()], |r| {
            r.get::<_, String>(0)
        })?;
        if let [text] = receipts.as_slice() {
            let receipt: crate::delegation_identity::OperationReceipt =
                serde_json::from_str(text).map_err(internal)?;
            match crate::engagement::intent_lookup(c, &receipt.namespace)? {
                crate::engagement::IntentLookup::Accepted(retained) if *retained == receipt => {
                    return Ok((
                        receipt.namespace.idempotency_key,
                        receipt
                            .accepted_input_digest
                            .trim_start_matches("sha256:")
                            .to_owned(),
                    ));
                }
                _ => return Err(integrity("materialized external receipt is invalid")),
            }
        }
        if !receipts.is_empty() {
            return Err(integrity("materialized external receipt is ambiguous"));
        }
    }
    let legacy = match kind {
        "open_external_engagement" => "open_external_engagement",
        "assign_external_specialist_task" => "assign_external_specialist_task",
        _ => "hire_external_specialist",
    };
    let field = match kind {
        "open_external_engagement" => "engagement_id",
        "assign_external_specialist_task" => "task_id",
        _ => "specialist_run_id",
    };
    let query = format!(
        "SELECT idempotency_key,request_sha256 FROM operations WHERE operation=?1 AND json_extract(response_json,'$.{field}')=?2"
    );
    let found = rows(c, &query, params![legacy, id.to_string()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    match found.as_slice() {
        [(key, digest)] => Ok((key.clone(), digest.clone())),
        _ => Err(integrity("materialized external operation is unbound")),
    }
}

fn externals(store: &OrchestrationStore, state: &mut State) -> Result<(), MachineError> {
    let c = &*store.connection;
    if !table_present(c, "engagements")? {
        return Ok(());
    }
    let ids = rows(
        c,
        "SELECT engagement_id FROM engagements WHERE workspace_id=?1 AND authority_kind='external_v1' ORDER BY engagement_id",
        [&state.workspace_id],
        |r| r.get::<_, String>(0),
    )?;
    for id in ids {
        let id = parse_uuid(&id)?;
        let (workspace, binding) = crate::engagement::external_owner_binding_on(c, id)?;
        if workspace != state.workspace_id {
            return Err(integrity("materialized external workspace differs"));
        }
        let raw:Value=c.query_row("SELECT json_object('bootstrap',bootstrap_operation_id,'external',external_controller_ref_json,'external_hash',external_controller_ref_sha256,'label',label,'state',state,'revision',revision,'created',created_at_ms,'updated',updated_at_ms) FROM engagements WHERE engagement_id=?1",[id.to_string()],|r|r.get::<_,String>(0)).map_err(internal).and_then(|x|serde_json::from_str(&x).map_err(internal))?;
        let external = match raw["external"].as_str() {
            Some(text) => {
                if raw["external_hash"].as_str() != Some(sha256_hex(text.as_bytes()).as_str()) {
                    return Err(integrity("external Controller reference digest differs"));
                }
                serde_json::from_str(text).map_err(internal)?
            }
            None => {
                if !raw["external"].is_null() || !raw["external_hash"].is_null() {
                    return Err(integrity("external Controller provenance differs"));
                }
                crate::engagement::accepted_external_owner_on(c, id)?;
                Value::Null
            }
        };
        let bootstrap = parse_uuid(string(&raw, "bootstrap")?)?;
        let created = integer(&raw, "created")?;
        let updated = integer(&raw, "updated")?;
        let (key, request) = external_operation(
            c,
            &workspace,
            "open_external_engagement",
            "engagement_ref",
            id,
        )?;
        let controller = aggregate_controller(binding.clone(), &workspace)?;
        state.aggregate_operations.push(AggregateOperation {
            operation_id: bootstrap,
            operation_kind: "open_external_engagement".into(),
            aggregate_kind: "external_specialist_engagement".into(),
            aggregate_id: id,
            idempotency_key: key,
            request_sha256: request,
            root_run_id: None,
            external_controller_ref: (!external.is_null()).then(|| external.clone()),
            controller_binding: Some(aggregate_controller(binding, &workspace)?),
            state: "ready".into(),
            safe_error_code: None,
            requested_at: timestamp(created)?,
            provisioning_started_at: None,
            ready_at: Some(timestamp(created)?),
            finished_at: Some(timestamp(created)?),
        });
        let mut members = Vec::new();
        for raw in value_rows(
            c,
            "SELECT json_object('run',specialist_run_id,'hire',hire_operation_id,'configuration',configuration_json,'digest',configuration_sha256,'role',role_ref,'state',state,'residency',actor_residency,'created',created_at_ms,'updated',updated_at_ms) FROM members WHERE engagement_id=?1 ORDER BY specialist_run_id",
            [id.to_string()],
        )? {
            let run = parse_uuid(string(&raw, "run")?)?;
            let hire = parse_uuid(string(&raw, "hire")?)?;
            let text = string(&raw, "configuration")?;
            let configuration: crate::run::AgentConfigurationSnapshot =
                serde_json::from_str(text).map_err(internal)?;
            if crate::run::agent_configuration_digest(&configuration).map_err(internal)?
                != string(&raw, "digest")?
            {
                return Err(integrity("external configuration digest differs"));
            }
            let role_hash = if let Some(instructions) = configuration.instructions.delegation() {
                configuration.validate_delegation().map_err(internal)?;
                instructions
                    .role_snapshot
                    .digest()?
                    .trim_start_matches("sha256:")
                    .to_owned()
            } else if let Some(instructions) = configuration.instructions.caller() {
                instructions.role.sha256.clone()
            } else {
                sha256_hex(string(&raw, "role")?.as_bytes())
            };
            let member_state = string(&raw, "state")?.to_owned();
            let joined = integer(&raw, "created")?;
            let changed = integer(&raw, "updated")?;
            let (key, request) =
                external_operation(c, &workspace, "hire_external_specialist", "member_ref", run)?;
            let spawn_state = match member_state.as_str() {
                "active" | "releasing" | "released" => "ready",
                "provisioning" => "provisioning",
                "degraded" => "failed",
                _ => return Err(integrity("external member state is unsupported")),
            };
            state.spawn_operations.push(SpawnOperation {
                operation_id: hire,
                aggregate_kind: "external_specialist_engagement".into(),
                aggregate_id: id,
                idempotency_key: key,
                parent_run_id: None,
                child_run_id: Some(run),
                approval_request_id: None,
                requested_spec_sha256: request,
                state: spawn_state.into(),
                safe_error_code: None,
                requested_at: timestamp(joined)?,
                provisioning_started_at: None,
                ready_at: None,
                finished_at: None,
            });
            members.push(ExternalMember {
                run_id: run,
                origin: "external_hire".into(),
                runtime_owner: "dolgorae".into(),
                semantic_owner: "external_controller".into(),
                role_ref: string(&raw, "role")?.into(),
                role_snapshot_sha256: role_hash,
                agent_configuration_sha256: string(&raw, "digest")?.into(),
                hire_operation_id: hire,
                membership_state: member_state.clone(),
                actor_residency: string(&raw, "residency")?.into(),
                activation_policy: "on_external_request".into(),
                current_activation_operation_id: current_activation(c, run)?,
                joined_at: timestamp(joined)?,
                released_at: (member_state == "released")
                    .then(|| timestamp(changed))
                    .transpose()?,
            });
        }
        state.external_engagements.push(External {
            aggregate_kind: "external_specialist_engagement".into(),
            semantic_owner: "external_controller".into(),
            engagement_id: id,
            bootstrap_operation_id: bootstrap,
            external_controller_ref: external,
            controller_binding: controller,
            label: raw["label"].as_str().map(str::to_owned),
            status: string(&raw, "state")?.into(),
            revision: raw["revision"]
                .as_u64()
                .ok_or_else(|| integrity("external revision is invalid"))?,
            members,
            created_at: timestamp(created)?,
            updated_at: timestamp(updated)?,
        });
    }
    Ok(())
}

fn string<'a>(row: &'a Value, key: &str) -> Result<&'a str, MachineError> {
    row[key]
        .as_str()
        .ok_or_else(|| integrity("materialized required text is absent"))
}
fn integer(row: &Value, key: &str) -> Result<i64, MachineError> {
    row[key]
        .as_i64()
        .ok_or_else(|| integrity("materialized required timestamp is absent"))
}
fn value_rows(
    c: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<Value>, MachineError> {
    rows(c, sql, parameters, |r| r.get::<_, String>(0))?
        .into_iter()
        .map(|text| serde_json::from_str(&text).map_err(internal))
        .collect()
}
fn priority(value: u8) -> Result<String, MachineError> {
    match value {
        0 => Ok("interactive".into()),
        1 => Ok("normal".into()),
        2 => Ok("background".into()),
        _ => Err(integrity("materialized priority is invalid")),
    }
}

struct Claim {
    owner: Option<String>,
    until: Option<String>,
}
fn claim(c: &Connection, work: Uuid) -> Result<Claim, MachineError> {
    if !table_present(c, "scheduler_claims")? {
        return Ok(Claim {
            owner: None,
            until: None,
        });
    }
    let row: Option<(String, i64)> = c
        .query_row(
            "SELECT claim_id,lease_until_ms FROM scheduler_claims WHERE work_ref=?1",
            [work.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(internal)?;
    row.map(|(owner, until)| {
        Ok(Claim {
            owner: Some(owner),
            until: Some(timestamp(until)?),
        })
    })
    .unwrap_or(Ok(Claim {
        owner: None,
        until: None,
    }))
}
fn causal(
    c: &Connection,
    external: bool,
    aggregate: Uuid,
    kind: &str,
    payload: &str,
) -> Result<Option<(u64, i64)>, MachineError> {
    let (table, scope) = if external {
        ("events", "engagement_id")
    } else {
        ("orchestration_events", "session_id")
    };
    let payload = if external {
        sha256_hex(payload.as_bytes())
    } else {
        payload.to_owned()
    };
    let query = format!(
        "SELECT sequence,created_at_ms FROM {table} WHERE {scope}=?1 AND kind=?2 AND payload_sha256=?3 ORDER BY sequence"
    );
    let values = rows(
        c,
        &query,
        params![aggregate.to_string(), kind, payload],
        |r| Ok((r.get::<_, u64>(0)?, r.get::<_, i64>(1)?)),
    )?;
    match values.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        _ => Err(integrity("materialized causal identity is ambiguous")),
    }
}
fn tasks(store: &OrchestrationStore, state: &mut State) -> Result<(), MachineError> {
    let c = &*store.connection;
    let managed = state
        .orchestrated_sessions
        .iter()
        .map(|s| s.session_id)
        .collect::<Vec<_>>();
    let external = state
        .external_engagements
        .iter()
        .map(|s| s.engagement_id)
        .collect::<Vec<_>>();
    for (is_external, aggregate) in managed
        .into_iter()
        .map(|id| (false, id))
        .chain(external.into_iter().map(|id| (true, id)))
    {
        let query = if is_external {
            "SELECT json_object('id',task_id,'target',specialist_run_id,'turn',turn_id,'external',external_request_ref_json,'request',request_json,'deadline',deadline_seconds,'state',state,'artifact',artifact_id,'error',safe_error_code,'created',created_at_ms,'updated',updated_at_ms) FROM tasks WHERE engagement_id=?1 ORDER BY task_id"
        } else {
            "SELECT json_object('id',task_id,'key',idempotency_key,'target',target_run_id,'turn',target_turn_id,'source',source_run_id,'source_turn',source_turn_id,'request',request_json,'state',state,'artifact',result_artifact_id,'error',safe_error_code,'created',created_at_ms,'updated',updated_at_ms) FROM brokered_tasks WHERE session_id=?1 ORDER BY task_id"
        };
        for raw in value_rows(c, query, [aggregate.to_string()])? {
            let id = parse_uuid(string(&raw, "id")?)?;
            let target = parse_uuid(string(&raw, "target")?)?;
            let created = integer(&raw, "created")?;
            let request: Value =
                serde_json::from_str(string(&raw, "request")?).map_err(internal)?;
            let (key, source, source_turn, external_ref, deadline) = if is_external {
                let reference: Value =
                    serde_json::from_str(string(&raw, "external")?).map_err(internal)?;
                let registered=table_present(c,"delegation_operations")? && c.query_row("SELECT EXISTS(SELECT 1 FROM delegation_operations WHERE operation_kind='assign_external_specialist_task' AND state='accepted' AND json_extract(accepted_subjects_json,'$.task_ref')=?1)",[id.to_string()],|r|r.get::<_,bool>(0)).map_err(internal)?;
                let (key, original_digest) = external_operation(
                    c,
                    &state.workspace_id,
                    "assign_external_specialist_task",
                    "task_ref",
                    id,
                )?;
                if registered {
                    crate::scheduler::retained_assignment(
                        c,
                        crate::assignment_queue::Owner::External(aggregate),
                        id,
                        target,
                        &state.workspace_id,
                        &store.state_root,
                    )?;
                } else if original_digest != sha256_hex(string(&raw, "request")?.as_bytes()) {
                    return Err(integrity("historical external request digest differs"));
                }
                let external = if registered {
                    let accepted = crate::engagement::external_task_identity_on(c, aggregate, id)?
                        .ok_or_else(|| integrity("registered external task identity is absent"))?;
                    if reference
                        != serde_json::json!({"provisioning_ref":null,"accepted_task_digest":accepted.digest()?,"deadline_seconds":integer(&raw,"deadline")?})
                    {
                        return Err(integrity("external accepted request marker differs"));
                    }
                    None
                } else {
                    Some(reference)
                };
                (
                    key,
                    None,
                    None,
                    external,
                    created
                        .checked_add(
                            integer(&raw, "deadline")?
                                .checked_mul(1000)
                                .ok_or_else(|| integrity("task deadline overflows"))?,
                        )
                        .ok_or_else(|| integrity("task deadline overflows"))?,
                )
            } else {
                let source = parse_uuid(string(&raw, "source")?)?;
                let deadline = if let Some(retained) =
                    super::policy_task::load(c, &store.state_root, aggregate, id)?
                {
                    retained.capture.deadline_ms
                } else if request["schema"].as_str() == Some(super::SUCCESSOR_TASK_MARKER_SCHEMA) {
                    crate::scheduler::retained_assignment(
                        c,
                        crate::assignment_queue::Owner::Managed(aggregate),
                        id,
                        target,
                        &state.workspace_id,
                        &store.state_root,
                    )?
                    .deadline_ms
                } else {
                    let accepted = store.accepted_task(id)?;
                    if accepted.specialist_run_id != target
                        || accepted.objective != request["objective"]
                        || accepted.requested_access != request["requested_access"]
                    {
                        return Err(integrity("historical managed request differs"));
                    }
                    checked_deadline_ms(accepted.deadline_origin_ms, accepted.deadline_seconds)?
                };
                (
                    string(&raw, "key")?.into(),
                    Some(source),
                    Some(string(&raw, "source_turn")?.into()),
                    None,
                    deadline,
                )
            };
            let task_state = string(&raw, "state")?.to_owned();
            let claim = claim(c, id)?;
            let queued = causal(
                c,
                is_external,
                aggregate,
                "assignment_queued",
                &sha256_hex(id.as_bytes()),
            )?;
            let queued_priority = if table_present(c, "assignment_queue_entries")? {
                c.query_row(
                    "SELECT priority FROM assignment_queue_entries WHERE task_id=?1 AND scope_ref=?2 AND target_run_id=?3 AND deadline_ms=?4",
                    params![id.to_string(),aggregate.to_string(),target.to_string(),deadline],
                    |r| r.get::<_, u8>(0),
                ).optional().map_err(internal)?
            } else {
                None
            };
            let retained_priority = if is_external {
                crate::assignment_queue::external_order_priority(
                    c, aggregate, id, target, deadline,
                )?
            } else {
                crate::collaboration::queued_root_priority(c, aggregate, id, target)?
            };
            if queued_priority
                .zip(retained_priority)
                .is_some_and(|(a, b)| a != b)
            {
                return Err(integrity("accepted task priority differs"));
            }
            let priority_value = queued_priority
                .or(retained_priority)
                .or_else(|| {
                    // Historical immediate admissions never entered a priority queue.
                    queued.is_none().then_some(1)
                })
                .ok_or_else(|| integrity("original queued task priority is unavailable"))?;
            let completion = if !is_external
                && table_present(c, "brokered_result_publication_sequence")?
            {
                c.query_row("SELECT published_at_ms FROM brokered_result_publication_sequence WHERE task_id=?1",[id.to_string()],|r|r.get::<_,i64>(0)).optional().map_err(internal)?.map(timestamp).transpose()?
            } else {
                None
            };
            let delivery_table = if is_external {
                "delivery_receipts"
            } else {
                "brokered_delivery_receipts"
            };
            let delivered = if table_present(c, delivery_table)? {
                let query =
                    format!("SELECT delivered_at_ms FROM {delivery_table} WHERE task_id=?1");
                c.query_row(&query, [id.to_string()], |r| r.get::<_, i64>(0))
                    .optional()
                    .map_err(internal)?
                    .map(timestamp)
                    .transpose()?
            } else {
                None
            };
            let item_state = match task_state.as_str() {
                "queued" => "queued",
                "accepted" | "claimed" | "dispatching" => "claimed",
                "cancelled" | "expired" => "cancelled",
                _ => "delivered",
            };
            if let Some((sequence, time)) = queued {
                state.mailbox_items.push(Mailbox {
                    aggregate_kind: if is_external {
                        "external_specialist_engagement"
                    } else {
                        "orchestrated_session"
                    }
                    .into(),
                    aggregate_id: aggregate,
                    mailbox_run_id: target,
                    mailbox_sequence: sequence,
                    item_kind: "specialist_task_request".into(),
                    related_id: id,
                    priority_class: priority(priority_value)?,
                    blocks_source_turn: false,
                    item_state: item_state.into(),
                    lease_owner: claim.owner.clone(),
                    lease_until: claim.until.clone(),
                    created_at: timestamp(time)?,
                    delivered_at: None,
                    acknowledged_at: None,
                });
            }
            state.specialist_tasks.push(Task {
                task_id: id,
                task_kind: if is_external {
                    "external_assignment"
                } else {
                    "brokered_delegation"
                }
                .into(),
                aggregate_id: aggregate,
                idempotency_key: key,
                source_run_id: source,
                source_turn_id: source_turn,
                external_request_ref: external_ref,
                target_run_id: target,
                target_turn_id: raw["turn"].as_str().map(str::to_owned),
                request_artifact_ref: id,
                result_artifact_ref: raw["artifact"].as_str().map(parse_uuid).transpose()?,
                priority_class: priority(priority_value)?,
                state: task_state,
                safe_error_code: raw["error"].as_str().map(str::to_owned),
                created_at: timestamp(created)?,
                deadline_at: Some(timestamp(deadline)?),
                claim_lease_until: claim.until,
                claim_owner: claim.owner,
                claimed_at: None,
                queued_at: queued.map(|(_, time)| timestamp(time)).transpose()?,
                dispatched_at: None,
                completed_at: completion,
                delivered_at: delivered,
            });
        }
    }
    Ok(())
}

fn exchanges(store: &OrchestrationStore, state: &mut State) -> Result<(), MachineError> {
    let c = &*store.connection;
    if !table_present(c, "collaboration_exchanges")? {
        return Ok(());
    }
    for session in &state.orchestrated_sessions {
        for raw in value_rows(
            c,
            "SELECT json_object('id',e.exchange_id,'root',e.root_task_id,'source',e.source_run_id,'source_turn',e.source_turn_id,'call',e.source_call_id,'key',e.idempotency_key,'target',e.target_run_id,'request',e.request_json,'contexts',e.context_json,'priority',e.root_priority,'deadline',e.deadline_ms,'execution',e.execution_state,'delivery',e.delivery_state,'created',e.created_at_ms,'parent',l.parent_exchange_id,'depth',coalesce(l.hop_depth,1),'thread',x.thread_id,'turn',x.turn_id,'artifact',r.artifact_id,'error',r.safe_error_code,'result_created',r.created_at,'ack',o.acknowledged_at_ms) FROM collaboration_exchanges e LEFT JOIN collaboration_lineage l USING(exchange_id) LEFT JOIN collaboration_execution_links x USING(exchange_id) LEFT JOIN collaboration_results r USING(exchange_id) LEFT JOIN collaboration_result_outbox o USING(exchange_id) WHERE e.session_id=?1 ORDER BY e.exchange_id",
            [session.session_id.to_string()],
        )? {
            let id = parse_uuid(string(&raw, "id")?)?;
            let record = crate::collaboration::execution_record(c, id)?;
            let (sequence, time) = causal(
                c,
                false,
                session.session_id,
                "collaboration_queued",
                &record.event_digest,
            )?
            .ok_or_else(|| integrity("collaboration enqueue identity is absent"))?;
            let source = parse_uuid(string(&raw, "source")?)?;
            let target = parse_uuid(string(&raw, "target")?)?;
            let artifact = raw["artifact"].as_str().map(parse_uuid).transpose()?;
            let execution = string(&raw, "execution")?;
            let delivery = string(&raw, "delivery")?;
            if execution == "succeeded" {
                let artifact = artifact
                    .ok_or_else(|| integrity("successful collaboration artifact is absent"))?;
                crate::collaboration::artifact_metadata(c, source, artifact)?
                    .ok_or_else(|| integrity("collaboration publication is absent"))?;
                crate::collaboration::artifact_content(c, source, artifact)?
                    .ok_or_else(|| integrity("collaboration bytes are absent"))?;
            }
            let blocking = crate::collaboration::dependency_unblock(c, id)?;
            let context: Vec<Value> =
                serde_json::from_str(string(&raw, "contexts")?).map_err(internal)?;
            let claim = claim(c, id)?;
            let priority = priority(u8::try_from(integer(&raw, "priority")?).map_err(internal)?)?;
            let completed = raw["result_created"].as_str().map(str::to_owned);
            let ack = raw["ack"].as_i64().map(timestamp).transpose()?;
            state.mailbox_items.push(Mailbox {
                aggregate_kind: "orchestrated_session".into(),
                aggregate_id: session.session_id,
                mailbox_run_id: target,
                mailbox_sequence: sequence,
                item_kind: "collaboration_request".into(),
                related_id: id,
                priority_class: priority.clone(),
                blocks_source_turn: blocking,
                item_state: match execution {
                    "queued" => "queued",
                    "claimed" | "dispatching" => "claimed",
                    "cancelled" | "expired" => "cancelled",
                    _ => "delivered",
                }
                .into(),
                lease_owner: claim.owner.clone(),
                lease_until: claim.until.clone(),
                created_at: timestamp(time)?,
                delivered_at: None,
                acknowledged_at: None,
            });
            // Result sequence comes from the existing publication/expiry/unknown
            // event in this Session's domain, never the outbox's local counter.
            if delivery != "not_ready" {
                let event = if execution == "expired" {
                    causal(
                        c,
                        false,
                        session.session_id,
                        "collaboration_expired",
                        &sha256_hex(id.as_bytes()),
                    )?
                } else if let Some(thread) = raw["thread"].as_str() {
                    let result:Option<(Option<String>,Option<String>)>=c.query_row("SELECT sha256,safe_error_code FROM collaboration_results WHERE exchange_id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(internal)?;
                    if let Some((digest, error)) = result {
                        let payload = sha256_hex(
                            serde_json::json!([
                                id.to_string(),
                                target,
                                thread,
                                raw["turn"],
                                execution,
                                artifact,
                                digest,
                                error
                            ])
                            .to_string()
                            .as_bytes(),
                        );
                        causal(
                            c,
                            false,
                            session.session_id,
                            "collaboration_result_published",
                            &payload,
                        )?
                    } else {
                        unknown_result_event(c, session.session_id, id)?
                    }
                } else {
                    unknown_result_event(c, session.session_id, id)?
                };
                let (sequence, time) =
                    event.ok_or_else(|| integrity("result mailbox causal event is absent"))?;
                state.mailbox_items.push(Mailbox {
                    aggregate_kind: "orchestrated_session".into(),
                    aggregate_id: session.session_id,
                    mailbox_run_id: source,
                    mailbox_sequence: sequence,
                    item_kind: "collaboration_result".into(),
                    related_id: id,
                    priority_class: priority.clone(),
                    blocks_source_turn: false,
                    item_state: if delivery == "acknowledged" {
                        "acknowledged"
                    } else {
                        "queued"
                    }
                    .into(),
                    lease_owner: None,
                    lease_until: None,
                    created_at: timestamp(time)?,
                    delivered_at: ack.clone(),
                    acknowledged_at: ack.clone(),
                });
            }
            state.collaboration_exchanges.push(Exchange {
                exchange_id: id,
                session_id: session.session_id,
                root_task_id: parse_uuid(string(&raw, "root")?)?,
                parent_exchange_id: raw["parent"].as_str().map(parse_uuid).transpose()?,
                idempotency_key: string(&raw, "key")?.into(),
                source_run_id: source,
                source_turn_id: string(&raw, "source_turn")?.into(),
                source_tool_call_id: string(&raw, "call")?.into(),
                target_run_id: target,
                target_turn_id: raw["turn"].as_str().map(str::to_owned),
                request_artifact_ref: id,
                context_manifest_ref: (!context.is_empty()).then_some(id),
                result_artifact_ref: artifact,
                priority_class: priority,
                blocks_source_turn: blocking,
                hop_depth: raw["depth"]
                    .as_u64()
                    .ok_or_else(|| integrity("collaboration depth is invalid"))?,
                enqueue_sequence: sequence,
                execution_state: execution.into(),
                delivery_state: delivery.into(),
                safe_error_code: raw["error"].as_str().map(str::to_owned).or_else(|| {
                    (execution == "interrupted_unknown").then(|| "INTERRUPTED_UNKNOWN".into())
                }),
                claim_owner: claim.owner,
                claim_lease_until: claim.until,
                deadline_at: Some(timestamp(record.deadline_ms)?),
                created_at: timestamp(integer(&raw, "created")?)?,
                claimed_at: None,
                dispatched_at: None,
                completed_at: completed,
                acknowledged_at: ack,
            });
        }
    }
    Ok(())
}
fn unknown_result_event(
    c: &Connection,
    session: Uuid,
    exchange: Uuid,
) -> Result<Option<(u64, i64)>, MachineError> {
    let claim: Option<String> = if table_present(c, "scheduler_claims")? {
        c.query_row("SELECT claim_id FROM scheduler_claims WHERE work_ref=?1 AND kind='consultation' AND phase='unknown'",[exchange.to_string()],|r|r.get(0)).optional().map_err(internal)?
    } else {
        None
    };
    claim
        .map(|claim| {
            causal(
                c,
                false,
                session,
                "scheduler_dispatch_observed",
                &sha256_hex(format!("{claim}\0unknown").as_bytes()),
            )
        })
        .transpose()
        .map(Option::flatten)
}

fn activations(c: &Connection, state: &mut State) -> Result<(), MachineError> {
    if !table_present(c, "scheduler_actors")? {
        return Ok(());
    }
    let mut seen = BTreeSet::new();
    if table_present(c, "scheduler_operations")? {
        for raw in value_rows(
            c,
            "SELECT json_object('id',operation_id,'run',target_run_id,'kind',scope_kind,'scope',scope_ref,'until',lease_until_ms,'attempt',attempt,'state',state,'error',safe_blocker,'identity',identity_json,'digest',identity_sha256) FROM scheduler_operations WHERE kind='activation' ORDER BY operation_id",
            [],
        )? {
            let id = parse_uuid(string(&raw, "id")?)?;
            seen.insert(id);
            if sha256_hex(string(&raw, "identity")?.as_bytes()) != string(&raw, "digest")? {
                return Err(integrity("activation capture digest differs"));
            }
            activation(c, state, &raw, id)?;
        }
    }
    for raw in value_rows(
        c,
        "SELECT json_object('id',operation_id,'run',target_run_id,'kind',scope_kind,'scope',scope_ref,'until',lease_until_ms,'attempt',attempts,'state','prepared','error',safe_blocker) FROM scheduler_actors WHERE operation_kind='activation' AND operation_id IS NOT NULL ORDER BY operation_id",
        [],
    )? {
        let id = parse_uuid(string(&raw, "id")?)?;
        if seen.insert(id) {
            activation(c, state, &raw, id)?;
        }
    }
    Ok(())
}
fn activation(
    c: &Connection,
    state: &mut State,
    raw: &Value,
    id: Uuid,
) -> Result<(), MachineError> {
    let external = string(raw, "kind")? == "external_engagement";
    let scope = parse_uuid(string(raw, "scope")?)?;
    if !(if external {
        state
            .external_engagements
            .iter()
            .any(|s| s.engagement_id == scope)
    } else {
        state
            .orchestrated_sessions
            .iter()
            .any(|s| s.session_id == scope)
    }) {
        return Ok(());
    }
    let (_, created) = causal(
        c,
        external,
        scope,
        "actor_activation_started",
        &sha256_hex(id.to_string().as_bytes()),
    )?
    .ok_or_else(|| integrity("activation request event is absent"))?;
    let phase = string(raw, "state")?;
    let terminal = match phase {
        "resident" => Some("ready"),
        "unavailable" => Some("failed"),
        "prepared" => None,
        _ => return Err(integrity("activation state is unsupported")),
    };
    let completed = if terminal.is_some() {
        causal(
            c,
            external,
            scope,
            "actor_operation_outcome",
            &sha256_hex(format!("{id}\0{phase}").as_bytes()),
        )?
        .map(|(_, time)| timestamp(time))
        .transpose()?
    } else {
        None
    };
    state.activation_operations.push(Activation {
        activation_id: id,
        aggregate_kind: if external {
            "external_specialist_engagement"
        } else {
            "orchestrated_session"
        }
        .into(),
        aggregate_id: scope,
        run_id: parse_uuid(string(raw, "run")?)?,
        trigger_kind: if external {
            "external_request"
        } else {
            "mailbox"
        }
        .into(),
        state: terminal.unwrap_or("activating").into(),
        attempt: raw["attempt"]
            .as_u64()
            .ok_or_else(|| integrity("activation attempt is invalid"))?,
        lease_owner: Some(id.to_string()),
        lease_until: Some(timestamp(integer(raw, "until")?)?),
        safe_error_code: raw["error"].as_str().map(str::to_owned),
        requested_at: timestamp(created)?,
        started_at: None,
        completed_at: completed,
    });
    Ok(())
}

fn validate(state: &State, root: &Path) -> Result<(), MachineError> {
    validate_closed_fields(state)?;
    if state.schema_version != 2
        || state.revision != state.head_sequence
        || (state.head_sequence == 0) != state.head_hash.is_none()
    {
        return Err(integrity("checked state head is invalid"));
    }
    if let Some(hash) = &state.head_hash {
        digest(hash, "checked state head")?;
    }
    let mut membership = BTreeSet::new();
    let mut aggregate_ids = BTreeSet::new();
    let mut members = BTreeSet::new();
    let mut spawns = BTreeSet::new();
    for op in &state.spawn_operations {
        if !spawns.insert(op.operation_id) {
            return Err(integrity("spawn operation identity is duplicated"));
        }
    }
    for session in &state.orchestrated_sessions {
        if !aggregate_ids.insert(session.session_id) || session.root_run_id != session.session_id {
            return Err(integrity("Session identity is invalid"));
        }
        session.specialist_policy.validate(root)?;
        if session.specialist_policy.digest()? != session.specialist_policy_sha256 {
            return Err(integrity("Session policy digest differs"));
        }
        for member in &session.members {
            if !members.insert(member.run_id)
                || member.parent_run_id != session.root_run_id
                || !spawns.contains(&member.spawn_operation_id)
            {
                return Err(integrity("managed member linkage is invalid"));
            }
            if !terminal_state(&session.status)
                && member.membership_state != "retired"
                && !membership.insert(member.run_id)
            {
                return Err(integrity("Run belongs to multiple active aggregates"));
            }
            if crate::run::agent_configuration_digest(&member.agent_configuration)
                .map_err(internal)?
                != member.agent_configuration_sha256
            {
                return Err(integrity("member configuration hash differs"));
            }
            let (hash, configuration) = match &member.role_snapshot {
                CapturedRole::Policy(role) => (digest_value(role)?, role.agent_configuration()),
                CapturedRole::Delegated(role) => {
                    if member.activation_policy != "successor"
                        || member.agent_configuration.schema_version != 4
                    {
                        return Err(integrity("independent Role marker differs"));
                    }
                    (
                        role.digest()?.trim_start_matches("sha256:").into(),
                        &member.agent_configuration,
                    )
                }
            };
            if hash != member.role_snapshot_sha256 || configuration != &member.agent_configuration {
                return Err(integrity("member Role preimage differs"));
            }
        }
    }
    for engagement in &state.external_engagements {
        if !aggregate_ids.insert(engagement.engagement_id) {
            return Err(integrity("aggregate identity is duplicated"));
        }
        for member in &engagement.members {
            if !members.insert(member.run_id) || !spawns.contains(&member.hire_operation_id) {
                return Err(integrity("external member linkage is invalid"));
            }
            if !terminal_state(&engagement.status)
                && member.membership_state != "released"
                && !membership.insert(member.run_id)
            {
                return Err(integrity("Run belongs to multiple active aggregates"));
            }
        }
    }
    let mut tasks = BTreeSet::new();
    for task in &state.specialist_tasks {
        if !tasks.insert(task.task_id)
            || !aggregate_ids.contains(&task.aggregate_id)
            || !members.contains(&task.target_run_id)
            || task.request_artifact_ref != task.task_id
        {
            return Err(integrity("task linkage is invalid"));
        }
    }
    let mut exchanges = BTreeSet::new();
    for exchange in &state.collaboration_exchanges {
        if !exchanges.insert(exchange.exchange_id)
            || !tasks.contains(&exchange.root_task_id)
            || exchange.source_run_id == exchange.target_run_id
            || !members.contains(&exchange.source_run_id)
            || !members.contains(&exchange.target_run_id)
            || !(1..=8).contains(&exchange.hop_depth)
        {
            return Err(integrity("exchange linkage is invalid"));
        }
        if exchange.execution_state == "succeeded"
            && (exchange.result_artifact_ref.is_none()
                || !matches!(exchange.delivery_state.as_str(), "pending" | "acknowledged"))
        {
            return Err(integrity("successful exchange publication is invalid"));
        }
    }
    let mut mailbox = BTreeSet::new();
    for item in &state.mailbox_items {
        if item.mailbox_sequence == 0
            || !mailbox.insert((item.mailbox_run_id, item.mailbox_sequence))
            || !members.contains(&item.mailbox_run_id)
            || !(tasks.contains(&item.related_id) || exchanges.contains(&item.related_id))
        {
            return Err(integrity("mailbox identity is invalid"));
        }
    }
    let mut activations = BTreeSet::new();
    for op in &state.activation_operations {
        if !activations.insert(op.activation_id)
            || !members.contains(&op.run_id)
            || !aggregate_ids.contains(&op.aggregate_id)
        {
            return Err(integrity("activation linkage is invalid"));
        }
    }
    for session in &state.orchestrated_sessions {
        for member in &session.members {
            if member
                .current_activation_operation_id
                .is_some_and(|id| !activations.contains(&id))
            {
                return Err(integrity("member activation reference is absent"));
            }
        }
    }
    for engagement in &state.external_engagements {
        for member in &engagement.members {
            if member
                .current_activation_operation_id
                .is_some_and(|id| !activations.contains(&id))
            {
                return Err(integrity("member activation reference is absent"));
            }
        }
    }
    Ok(())
}

/// Closed private state vocabulary, independent of stringly SQL storage.
fn validate_closed_fields(state: &State) -> Result<(), MachineError> {
    let enumeration = |value: &str, allowed: &[&str]| {
        if allowed.contains(&value) {
            Ok(())
        } else {
            Err(integrity("checked state vocabulary is unsupported"))
        }
    };
    let time = |value: &str| {
        if crate::audit::is_microsecond_utc_timestamp(value) {
            Ok(())
        } else {
            Err(integrity("checked state timestamp is invalid"))
        }
    };
    let residency = |value: &str| {
        enumeration(
            value,
            &[
                "unstarted",
                "resident",
                "passivating",
                "passivated",
                "activating",
                "recovering",
                "unavailable",
                "terminal",
            ],
        )
    };
    let aggregate = |value: &str| {
        enumeration(
            value,
            &["orchestrated_session", "external_specialist_engagement"],
        )
    };
    let priority = |value: &str| enumeration(value, &["interactive", "normal", "background"]);
    let status = |value: &str| {
        enumeration(
            value,
            &[
                "creating",
                "active",
                "degraded",
                "recovering",
                "completing",
                "aborting",
                "completed",
                "aborted",
            ],
        )
    };
    if state.workspace_id.len() != 64
        || !state
            .workspace_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(integrity("checked workspace identity is invalid"));
    }
    for operation in &state.aggregate_operations {
        aggregate(&operation.aggregate_kind)?;
        enumeration(
            &operation.operation_kind,
            &["create_orchestrated_session", "open_external_engagement"],
        )?;
        enumeration(
            &operation.state,
            &[
                "requested",
                "provisioning",
                "ready",
                "failed",
                "cancelled",
                "recovery_required",
            ],
        )?;
        digest(&operation.request_sha256, "aggregate request")?;
        checked(&operation.idempotency_key, 256, "aggregate key")?;
        time(&operation.requested_at)?;
        if operation.aggregate_kind == "orchestrated_session" {
            if operation.root_run_id != Some(operation.aggregate_id)
                || operation.external_controller_ref.is_some()
                || operation.controller_binding.is_some()
            {
                return Err(integrity("managed bootstrap fields differ"));
            }
        } else if operation.root_run_id.is_some() || operation.controller_binding.is_none() {
            return Err(integrity("external bootstrap fields differ"));
        }
    }
    for session in &state.orchestrated_sessions {
        status(&session.status)?;
        enumeration(&session.aggregate_kind, &["orchestrated_session"])?;
        enumeration(&session.semantic_owner, &["dolgorae"])?;
        enumeration(
            &session.composition_state,
            &["standalone_primary", "brokered_hierarchy"],
        )?;
        enumeration(
            &session.approval_policy,
            &["fully_delegated", "user_approval_required"],
        )?;
        if session.revision == 0 {
            return Err(integrity("Session revision is invalid"));
        }
        time(&session.created_at)?;
        time(&session.updated_at)?;
        for member in &session.members {
            enumeration(&member.origin, &["brokered"])?;
            enumeration(&member.runtime_owner, &["dolgorae"])?;
            enumeration(&member.semantic_owner, &["dolgorae"])?;
            enumeration(
                &member.membership_state,
                &["provisioning", "active", "degraded", "retiring", "retired"],
            )?;
            residency(&member.actor_residency)?;
            enumeration(
                &member.activation_policy,
                &["successor", "on_mail", "manual", "keep_resident", "never"],
            )?;
            enumeration(
                &member.requested_access,
                &["read_only", "isolated_write", "canonical_workspace_write"],
            )?;
            digest(&member.role_snapshot_sha256, "member Role")?;
            digest(&member.agent_configuration_sha256, "member configuration")?;
            time(&member.joined_at)?;
            if (member.membership_state == "retired") != member.retired_at.is_some()
                || (member.membership_state == "retired" && member.actor_residency != "terminal")
            {
                return Err(integrity("retired member fields differ"));
            }
        }
    }
    for engagement in &state.external_engagements {
        status(&engagement.status)?;
        enumeration(
            &engagement.aggregate_kind,
            &["external_specialist_engagement"],
        )?;
        enumeration(&engagement.semantic_owner, &["external_controller"])?;
        if engagement.revision == 0 {
            return Err(integrity("engagement revision is invalid"));
        }
        time(&engagement.created_at)?;
        time(&engagement.updated_at)?;
        for member in &engagement.members {
            enumeration(&member.origin, &["external_hire"])?;
            enumeration(&member.runtime_owner, &["dolgorae"])?;
            enumeration(&member.semantic_owner, &["external_controller"])?;
            enumeration(
                &member.membership_state,
                &[
                    "provisioning",
                    "active",
                    "degraded",
                    "releasing",
                    "released",
                ],
            )?;
            residency(&member.actor_residency)?;
            enumeration(
                &member.activation_policy,
                &["on_external_request", "manual", "keep_resident", "never"],
            )?;
            digest(&member.role_snapshot_sha256, "external Role")?;
            digest(&member.agent_configuration_sha256, "external configuration")?;
            time(&member.joined_at)?;
            if (member.membership_state == "released") != member.released_at.is_some()
                || (member.membership_state == "released" && member.actor_residency != "terminal")
            {
                return Err(integrity("released member fields differ"));
            }
        }
    }
    for spawn in &state.spawn_operations {
        aggregate(&spawn.aggregate_kind)?;
        enumeration(
            &spawn.state,
            &[
                "requested",
                "awaiting_approval",
                "provisioning",
                "ready",
                "denied",
                "failed",
                "cancelled",
                "recovery_required",
            ],
        )?;
        digest(&spawn.requested_spec_sha256, "spawn request")?;
        checked(&spawn.idempotency_key, 256, "spawn key")?;
        time(&spawn.requested_at)?;
    }
    for task in &state.specialist_tasks {
        enumeration(
            &task.task_kind,
            &["brokered_delegation", "external_assignment"],
        )?;
        enumeration(
            &task.state,
            &[
                "accepted",
                "queued",
                "claimed",
                "dispatching",
                "running",
                "completed_not_delivered",
                "delivered",
                "failed",
                "interrupted_unknown",
                "cancelled",
                "expired",
            ],
        )?;
        checked(&task.idempotency_key, 256, "task key")?;
        priority(&task.priority_class)?;
        time(&task.created_at)?;
        if task.task_kind == "brokered_delegation" {
            if task.source_run_id.is_none()
                || task.source_turn_id.is_none()
                || task.external_request_ref.is_some()
            {
                return Err(integrity("managed task provenance is invalid"));
            }
        } else if task.source_run_id.is_some() || task.source_turn_id.is_some() {
            return Err(integrity("external task provenance is invalid"));
        }
    }
    for activation in &state.activation_operations {
        aggregate(&activation.aggregate_kind)?;
        enumeration(
            &activation.trigger_kind,
            &["mailbox", "external_request", "manual", "recovery"],
        )?;
        enumeration(
            &activation.state,
            &["requested", "activating", "ready", "failed", "cancelled"],
        )?;
        if !(1..=10).contains(&activation.attempt) {
            return Err(integrity("activation attempt is invalid"));
        }
        time(&activation.requested_at)?;
    }
    for exchange in &state.collaboration_exchanges {
        enumeration(
            &exchange.execution_state,
            &[
                "queued",
                "claimed",
                "dispatching",
                "running",
                "succeeded",
                "failed",
                "cancelled",
                "expired",
                "interrupted_unknown",
            ],
        )?;
        enumeration(
            &exchange.delivery_state,
            &["not_ready", "pending", "acknowledged"],
        )?;
        priority(&exchange.priority_class)?;
        time(&exchange.created_at)?;
    }
    for item in &state.mailbox_items {
        aggregate(&item.aggregate_kind)?;
        enumeration(
            &item.item_kind,
            &[
                "specialist_task_request",
                "collaboration_request",
                "collaboration_result",
            ],
        )?;
        enumeration(
            &item.item_state,
            &[
                "queued",
                "claimed",
                "delivered",
                "acknowledged",
                "cancelled",
            ],
        )?;
        priority(&item.priority_class)?;
        time(&item.created_at)?;
    }
    Ok(())
}

fn selected_basis(c: &Connection, workspace: &str) -> Result<String, MachineError> {
    let queries = [
        (
            "orchestrated_sessions",
            "SELECT json_array(session_id,status,policy_sha256,revision,created_at_ms,updated_at_ms) FROM orchestrated_sessions ORDER BY session_id",
        ),
        (
            "aggregate_bootstrap_operations",
            "SELECT json_array(operation_id,state,request_sha256,safe_error_code,updated_at_ms) FROM aggregate_bootstrap_operations ORDER BY operation_id",
        ),
        (
            "brokered_members",
            "SELECT json_array(run_id,role_snapshot_sha256,agent_configuration_sha256,controller_binding_json,membership_state,actor_residency,activation_policy,requested_access,updated_at_ms) FROM brokered_members ORDER BY run_id",
        ),
        (
            "brokered_spawn_operations",
            "SELECT json_array(operation_id,state,child_run_id,approval_request_id,request_sha256,safe_error_code,updated_at_ms) FROM brokered_spawn_operations ORDER BY operation_id",
        ),
        (
            "brokered_tasks",
            "SELECT json_array(task_id,request_sha256,state,target_turn_id,result_sha256,result_artifact_id,safe_error_code,updated_at_ms) FROM brokered_tasks ORDER BY task_id",
        ),
        (
            "brokered_task_acceptances",
            "SELECT json_array(task_id,accepted_request_sha256,source_request_sha256,deadline_origin_ms) FROM brokered_task_acceptances ORDER BY task_id",
        ),
        (
            "engagements",
            "SELECT json_array(engagement_id,state,controller_sha256,external_controller_ref_sha256,revision,updated_at_ms) FROM engagements ORDER BY engagement_id",
        ),
        (
            "members",
            "SELECT json_array(specialist_run_id,configuration_sha256,state,actor_residency,updated_at_ms) FROM members ORDER BY specialist_run_id",
        ),
        (
            "tasks",
            "SELECT json_array(task_id,objective_sha256,state,turn_id,result_sha256,artifact_id,safe_error_code,updated_at_ms) FROM tasks ORDER BY task_id",
        ),
        (
            "scheduler_claims",
            "SELECT json_array(claim_id,work_ref,phase,turn_id,lease_until_ms,safe_blocker) FROM scheduler_claims ORDER BY claim_id",
        ),
        (
            "scheduler_actors",
            "SELECT json_array(target_run_id,operation_id,operation_kind,lease_until_ms,attempts,safe_blocker,operation_snapshot_json) FROM scheduler_actors ORDER BY target_run_id",
        ),
        (
            "scheduler_operations",
            "SELECT json_array(operation_id,state,identity_sha256,observed_identity_json,safe_blocker) FROM scheduler_operations ORDER BY operation_id",
        ),
        (
            "collaboration_results",
            "SELECT json_array(exchange_id,artifact_id,byte_length,sha256,safe_error_code,created_at) FROM collaboration_results ORDER BY exchange_id",
        ),
        (
            "collaboration_result_outbox",
            "SELECT json_array(exchange_id,source_run_id,acknowledged_at_ms) FROM collaboration_result_outbox ORDER BY exchange_id",
        ),
        (
            "collaboration_exchanges",
            "SELECT json_array(exchange_id,request_sha256,execution_state,delivery_state,deadline_ms) FROM collaboration_exchanges ORDER BY exchange_id",
        ),
        (
            "external_assignment_order",
            "SELECT json_array(task_id,record_json,record_sha256) FROM external_assignment_order ORDER BY task_id",
        ),
        (
            "result_content_refs",
            "SELECT json_array(owner_kind,owner_id,representation,sha256,byte_length,retired_operation) FROM result_content_refs ORDER BY owner_kind,owner_id,representation",
        ),
    ];
    let mut selected = Vec::new();
    for (table, query) in queries {
        if table_present(c, table)? {
            selected.push((table, rows(c, query, [], |r| r.get::<_, String>(0))?));
        }
    }
    digest_value(
        &serde_json::json!({"head":head(c,workspace)?,"selected":selected,"waits":wait_basis(c)?}),
    )
}
fn capture_files(
    store: &OrchestrationStore,
) -> Result<
    BTreeMap<PathBuf, Option<crate::ledger_generation::ObservationFileIdentity>>,
    MachineError,
> {
    let c = &*store.connection;
    let mut runs = BTreeSet::new();
    for (table, query) in [
        (
            "orchestrated_sessions",
            "SELECT root_run_id FROM orchestrated_sessions",
        ),
        ("brokered_members", "SELECT run_id FROM brokered_members"),
        (
            "members",
            "SELECT specialist_run_id FROM members WHERE configuration_json IS NOT NULL",
        ),
    ] {
        if table_present(c, table)? {
            for id in rows(c, query, [], |r| r.get::<_, String>(0))? {
                runs.insert(parse_uuid(&id)?);
            }
        }
    }
    let mut files = BTreeMap::new();
    for run in runs {
        let dir = crate::run::run_root(&store.state_root, run);
        for path in [
            dir.clone(),
            dir.join("manifest.json"),
            dir.join("controller.json"),
            dir.join("state.json"),
        ] {
            let identity = match std::fs::symlink_metadata(&path) {
                Ok(_) => Some(
                    crate::ledger_generation::observation_file_identity(&path).map_err(internal)?,
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(internal(e)),
            };
            files.insert(path, identity);
        }
    }
    Ok(files)
}

fn verify_output_unchanged(
    directory: &PrivateDirectory,
    expected: Option<&OutputIdentity>,
) -> Result<(), MachineError> {
    let file = match DarwinSystem.openat_nofollow(&directory.file, OsStr::new("state.json"), false)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && expected.is_none() => return Ok(()),
        Err(_) => return Err(integrity("materialization destination moved")),
    };
    let metadata = file.metadata().map_err(internal)?;
    let Some(expected) = expected else {
        return Err(integrity("materialization destination appeared"));
    };
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.nlink() != 1
        || (metadata.dev(), metadata.ino(), metadata.len())
            != (expected.device, expected.inode, expected.length)
        || (metadata.mtime(), metadata.mtime_nsec()) != expected.modified
        || (metadata.ctime(), metadata.ctime_nsec()) != expected.changed
    {
        return Err(integrity("materialization destination moved"));
    }
    let named = DarwinSystem
        .statat_nofollow(&directory.file, OsStr::new("state.json"))
        .map_err(internal)?;
    if named.device != expected.device || named.inode != expected.inode {
        return Err(integrity("materialization destination moved"));
    }
    Ok(())
}
