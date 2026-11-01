//! Positive retained authority for Caller policy-born Primary task queues.
use super::{
    AcceptedSpecialistTask, AssignSpecialistTask, BrokeredMemberSnapshot, CompletedTaskOutput,
    OrchestratedSessionSnapshot, OrchestrationAdapter, OrchestrationStore, SpecialistTaskSnapshot,
    append_event, canonical_string, checked, checked_deadline_ms, digest_value,
    idempotency_conflict, integrity, internal, member_access_permits, now_ms, parse_uuid,
    policy_denied, require_current_policy_role, successor_member, table_present,
    validate_assign_task,
};
use crate::jcs::sha256_hex;
use crate::machine::{MachineError, new_uuid_v7};
use crate::primary_tool::PrimaryCallContext;
use crate::run::ControllerBinding;
use crate::session_policy::SessionPolicySnapshot;
use rusqlite::{Connection, Transaction};
use rusqlite::{OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

const SCHEMA: &str = "dolgorae.policy-task-dispatch/v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Capture {
    schema: String,
    pub session: Uuid,
    pub task: Uuid,
    pub target: Uuid,
    pub source_turn: String,
    pub source_call: String,
    source_thread: String,
    source_generation: u64,
    root_controller: ControllerBinding,
    member_controller: ControllerBinding,
    policy_sha256: String,
    role_sha256: String,
    configuration_sha256: String,
    pub(crate) request_sha256: String,
    pub(crate) accepted_sha256: String,
    pub effort: String,
    pub deadline_ms: i64,
    priority: u8,
}

pub(crate) struct Retained {
    pub capture: Capture,
    pub accepted: AcceptedSpecialistTask,
    pub member: BrokeredMemberSnapshot,
}

fn install(tx: &Transaction<'_>) -> Result<(), MachineError> {
    let version: Option<String> = tx
        .query_row(
            "SELECT value FROM metadata WHERE key='policy_task_queue_schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    match version.as_deref() {
        Some("1") => return Ok(()),
        Some(_) => return Err(integrity("policy task capture version is unsupported")),
        None => {}
    }
    tx.execute_batch("CREATE TABLE policy_task_dispatch_captures(
        task_id TEXT PRIMARY KEY REFERENCES brokered_tasks(task_id),
        capture_json TEXT NOT NULL,capture_sha256 TEXT NOT NULL,
        writer_capture_json TEXT,writer_capture_sha256 TEXT,writer_released INTEGER NOT NULL DEFAULT 0) STRICT;
        INSERT INTO metadata(key,value) VALUES('policy_task_queue_schema_version','1');"
    ).map_err(internal)
}

pub(crate) fn contains(c: &Connection, task: Uuid) -> Result<bool, MachineError> {
    if !table_present(c, "policy_task_dispatch_captures")? {
        return Ok(false);
    }
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM policy_task_dispatch_captures WHERE task_id=?1)",
        [task.to_string()],
        |r| r.get(0),
    )
    .map_err(internal)
}

/// Only a new Caller policy task enters this path. Old keys stay owned by the
/// original path, and provisioned tasks keep their closed facade identity.
pub(super) fn admit(
    store: &mut OrchestrationStore,
    context: &PrimaryCallContext,
    request: &AssignSpecialistTask,
    adapter: &mut impl OrchestrationAdapter,
    session: OrchestratedSessionSnapshot,
) -> Result<SpecialistTaskSnapshot, MachineError> {
    if let Some(old) = store.task_by_key(context.session_id, &context.idempotency_key)? {
        if store.task_request_digest(old.task_id)? != digest_value(request)? {
            return Err(idempotency_conflict(&context.idempotency_key));
        }
        load(
            &store.connection,
            &store.state_root,
            context.session_id,
            old.task_id,
        )?
        .ok_or_else(|| integrity("policy task capture is absent"))?;
        return Ok(old);
    }
    if !matches!(session.specialist_policy, SessionPolicySnapshot::Caller(_)) {
        return Err(integrity("policy queue requires a Caller policy capture"));
    }
    store.ensure_live_provider_policy(context.session_id)?;
    let member = store.member(context.session_id, request.specialist_run_id)?;
    require_current_policy_role(&session.specialist_policy, &member.role_ref)?;
    let role = session.specialist_policy.role(&member.role_ref)?;
    if member.membership_state != "active"
        || member.agent_configuration.schema_version != 3
        || member.agent_configuration.execution_lane != crate::domain::ExecutionLane::Dedicated
        || member.role_snapshot != role
        || member.agent_configuration != *role.agent_configuration()
        || !member_access_permits(&member.requested_access, &request.requested_access)
        || !role.allowed_access().contains(&request.requested_access)
    {
        return Err(policy_denied(
            &member.role_ref,
            "task is outside the accepted policy member authority",
        ));
    }
    let source = crate::snapshot::RunSnapshot::load(&store.state_root, context.source_run_id, 0)?;
    let target = crate::snapshot::RunSnapshot::load(&store.state_root, member.run_id, 0)?;
    let binding = source
        .manifest
        .aggregate_binding
        .as_ref()
        .ok_or_else(|| integrity("Primary aggregate binding is absent"))?;
    if source.manifest.run_id != session.root_run_id
        || binding.aggregate_id != session.session_id
        || binding.member_kind != crate::run::AggregateMemberKind::Primary
        || source.projection.active_turn_id.as_deref() != Some(context.source_turn_id.as_str())
        || target.manifest.agent_configuration != member.agent_configuration
        || target.manifest.controller != member.controller_binding
    {
        return Err(integrity(
            "policy task native source or target binding changed",
        ));
    }
    let effort = target
        .projection
        .default_effort
        .as_ref()
        .unwrap_or(&target.manifest.default_reasoning_effort)
        .clone();
    checked(&effort, 64, "task effort")?;
    let task_id = new_uuid_v7();
    let now = store.now_ms()?;
    let accepted = AcceptedSpecialistTask {
        task_id,
        specialist_run_id: member.run_id,
        objective: request.objective.clone(),
        contexts: adapter.resolve_task_contexts(context.source_run_id, &request.context_refs)?,
        expected_output: request.expected_output.clone(),
        requested_access: request.requested_access.clone(),
        deadline_origin_ms: now,
        deadline_seconds: request.deadline_seconds,
    };
    accepted.prompt()?;
    let accepted_json = canonical_string(&accepted)?;
    let accepted_sha256 = sha256_hex(accepted_json.as_bytes());
    let request_json = canonical_string(request)?;
    let request_sha256 = digest_value(request)?;
    let capture = Capture {
        schema: SCHEMA.into(),
        session: session.session_id,
        task: task_id,
        target: member.run_id,
        source_turn: context.source_turn_id.clone(),
        source_call: context.source_tool_call_id.clone(),
        source_thread: source
            .projection
            .thread_id
            .clone()
            .ok_or_else(|| integrity("Primary Thread is absent"))?,
        source_generation: source.projection.run_generation,
        root_controller: source.manifest.controller.clone(),
        member_controller: member.controller_binding.clone(),
        policy_sha256: session.specialist_policy_sha256.clone(),
        role_sha256: member.role_snapshot_sha256.clone(),
        configuration_sha256: member.agent_configuration_sha256.clone(),
        request_sha256: request_sha256.clone(),
        accepted_sha256: accepted_sha256.clone(),
        effort,
        deadline_ms: checked_deadline_ms(now, request.deadline_seconds)?,
        priority: 1,
    };
    let capture_json = canonical_string(&capture)?;
    let capture_sha256 = sha256_hex(capture_json.as_bytes());
    let tx = store.transaction()?;
    install(&tx)?;
    let current:(String,String,String,String)=tx.query_row(
        "SELECT s.status,s.policy_sha256,m.membership_state,m.agent_configuration_sha256 FROM orchestrated_sessions s JOIN brokered_members m USING(session_id) WHERE s.session_id=?1 AND m.run_id=?2",
        params![session.session_id.to_string(),member.run_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))
    ).map_err(internal)?;
    if current
        != (
            "active".into(),
            capture.policy_sha256.clone(),
            "active".into(),
            capture.configuration_sha256.clone(),
        )
    {
        return Err(integrity("policy task admission authority moved"));
    }
    tx.execute("INSERT INTO brokered_tasks(task_id,session_id,idempotency_key,request_sha256,
        source_run_id,source_turn_id,source_tool_call_id,target_run_id,request_json,state,created_at_ms,updated_at_ms)
        VALUES(?1,?2,?3,?4,?2,?5,?6,?7,?8,'queued',?9,?9)",
        params![task_id.to_string(),session.session_id.to_string(),context.idempotency_key,request_sha256,
            context.source_turn_id,context.source_tool_call_id,member.run_id.to_string(),request_json,now]
    ).map_err(internal)?;
    tx.execute("INSERT INTO brokered_task_acceptances(task_id,source_request_sha256,accepted_request_sha256,accepted_request_json,deadline_origin_ms)
        VALUES(?1,?2,?3,?4,?5)",params![task_id.to_string(),capture.request_sha256,accepted_sha256,accepted_json,now]).map_err(internal)?;
    append_event(
        &tx,
        session.session_id,
        "specialist_task_accepted",
        &capture.accepted_sha256,
        now,
    )?;
    tx.execute("INSERT INTO policy_task_dispatch_captures(task_id,capture_json,capture_sha256) VALUES(?1,?2,?3)",
        params![task_id.to_string(),capture_json,capture_sha256]).map_err(internal)?;
    append_event(
        &tx,
        session.session_id,
        "policy_task_dispatch_captured",
        &capture_sha256,
        now,
    )?;
    crate::collaboration::capture_policy_task_priority(&tx, &capture, now)?;
    crate::assignment_queue::admit(
        &tx,
        &crate::assignment_queue::Entry {
            owner: crate::assignment_queue::Owner::Managed(session.session_id),
            task: task_id,
            target: member.run_id,
            source: &context.source_run_id.to_string(),
            priority: 1,
            deadline_ms: capture.deadline_ms,
        },
    )?;
    crate::storage_observation::commit(tx).map_err(internal)?;
    crate::scheduler::notify(&store.state_root, member.run_id);
    store.task(task_id)
}

struct TaskRow {
    session: String,
    target: String,
    source: String,
    turn: String,
    call: String,
    request: String,
    request_sha256: String,
    accepted: String,
    accepted_sha256: String,
    origin: i64,
}

/// A missing row is absence of this lane, never permission to reinterpret a
/// closed facade receipt. Callers must retain the other owner's own checks.
pub(crate) fn load(
    c: &Connection,
    root: &Path,
    session: Uuid,
    task: Uuid,
) -> Result<Option<Retained>, MachineError> {
    if !contains(c, task)? {
        return Ok(None);
    }
    let (text,digest):(String,String)=c.query_row(
        "SELECT capture_json,capture_sha256 FROM policy_task_dispatch_captures WHERE task_id=?1",
        [task.to_string()],|r|Ok((r.get(0)?,r.get(1)?))
    ).map_err(internal)?;
    let capture: Capture =
        serde_json::from_str(&text).map_err(|_| integrity("policy task capture is invalid"))?;
    if capture.schema != SCHEMA
        || capture.session != session
        || capture.task != task
        || capture.priority != 1
        || capture.source_generation == 0
        || canonical_string(&capture)? != text
        || sha256_hex(text.as_bytes()) != digest
    {
        return Err(integrity("policy task capture binding differs"));
    }
    for value in [
        &capture.source_thread,
        &capture.source_turn,
        &capture.source_call,
        &capture.effort,
    ] {
        checked(value, 256, "captured task input")?;
    }
    let original:TaskRow=c.query_row(
        "SELECT t.session_id,t.target_run_id,t.source_run_id,t.source_turn_id,t.source_tool_call_id,
            t.request_json,t.request_sha256,a.accepted_request_json,a.accepted_request_sha256,a.deadline_origin_ms
         FROM brokered_tasks t JOIN brokered_task_acceptances a USING(task_id) WHERE task_id=?1",
        [task.to_string()],|r|Ok(TaskRow {session:r.get(0)?,target:r.get(1)?,source:r.get(2)?,
            turn:r.get(3)?,call:r.get(4)?,request:r.get(5)?,request_sha256:r.get(6)?,
            accepted:r.get(7)?,accepted_sha256:r.get(8)?,origin:r.get(9)?})
    ).map_err(internal)?;
    if original.session != session.to_string()
        || original.source != session.to_string()
        || original.target != capture.target.to_string()
        || original.turn != capture.source_turn
        || original.call != capture.source_call
        || original.request_sha256 != capture.request_sha256
        || original.accepted_sha256 != capture.accepted_sha256
        || sha256_hex(original.accepted.as_bytes()) != capture.accepted_sha256
    {
        return Err(integrity("policy task accepted identity differs"));
    }
    let request: AssignSpecialistTask =
        serde_json::from_str(&original.request).map_err(internal)?;
    validate_assign_task(&request)?;
    let accepted: AcceptedSpecialistTask =
        serde_json::from_str(&original.accepted).map_err(internal)?;
    if canonical_string(&accepted)? != original.accepted
        || digest_value(&request)? != capture.request_sha256
        || request.specialist_run_id != capture.target
        || accepted.task_id != task
        || accepted.specialist_run_id != capture.target
        || accepted.objective != request.objective
        || accepted.expected_output != request.expected_output
        || accepted.requested_access != request.requested_access
        || accepted.deadline_origin_ms != original.origin
        || accepted.deadline_seconds != request.deadline_seconds
        || checked_deadline_ms(original.origin, request.deadline_seconds)? != capture.deadline_ms
        || request.context_refs
            != accepted
                .contexts
                .iter()
                .map(|x| x.artifact_id)
                .collect::<Vec<_>>()
        || accepted.contexts.iter().any(|x| {
            x.byte_length != x.content.len() as u64 || sha256_hex(x.content.as_bytes()) != x.sha256
        })
    {
        return Err(integrity("policy task request preimage differs"));
    }
    accepted.prompt()?;
    let events:u64=c.query_row(
        "SELECT count(*) FROM orchestration_events WHERE session_id=?1 AND kind='policy_task_dispatch_captured' AND payload_sha256=?2",
        params![session.to_string(),digest],|r|r.get(0)
    ).map_err(internal)?;
    let accepted_event:bool=c.query_row(
        "SELECT EXISTS(SELECT 1 FROM orchestration_events WHERE session_id=?1 AND kind='specialist_task_accepted' AND payload_sha256=?2)",
        params![session.to_string(),capture.accepted_sha256],|r|r.get(0)
    ).map_err(internal)?;
    if events != 1 || !accepted_event {
        return Err(integrity("policy task causal authority differs"));
    }
    let policy_text: String = c
        .query_row(
            "SELECT policy_json FROM orchestrated_sessions WHERE session_id=?1",
            [session.to_string()],
            |r| r.get(0),
        )
        .map_err(internal)?;
    let policy: SessionPolicySnapshot = serde_json::from_str(&policy_text).map_err(internal)?;
    if !matches!(policy, SessionPolicySnapshot::Caller(_))
        || policy.digest()? != capture.policy_sha256
    {
        return Err(integrity("policy task Session capture differs"));
    }
    policy.validate(root)?;
    let member = successor_member(c, session, capture.target)?
        .ok_or_else(|| integrity("policy task member is absent"))?;
    let role = policy.role(&member.role_ref)?;
    if role.agent_configuration() != &member.agent_configuration
        || member.agent_configuration.schema_version != 3
        || digest_value(&role)? != capture.role_sha256
        || crate::run::agent_configuration_digest(&member.agent_configuration).map_err(internal)?
            != capture.configuration_sha256
    {
        return Err(integrity("policy task Role/configuration capture differs"));
    }
    let controller:String=c.query_row("SELECT controller_binding_json FROM brokered_members WHERE run_id=?1 AND session_id=?2",
        params![capture.target.to_string(),session.to_string()],|r|r.get(0)).map_err(internal)?;
    let controller: ControllerBinding = serde_json::from_str(&controller).map_err(internal)?;
    if controller != capture.member_controller {
        return Err(integrity("policy task Controller capture differs"));
    }
    let parent: String = c
        .query_row(
            "SELECT parent_run_id FROM brokered_members WHERE run_id=?1",
            [capture.target.to_string()],
            |r| r.get(0),
        )
        .map_err(internal)?;
    if parent != session.to_string() {
        return Err(integrity("policy task parent differs"));
    }
    let member = BrokeredMemberSnapshot {
        run_id: capture.target,
        parent_run_id: session,
        role_ref: member.role_ref,
        role_snapshot: role,
        role_snapshot_sha256: capture.role_sha256.clone(),
        agent_configuration: member.agent_configuration,
        agent_configuration_sha256: capture.configuration_sha256.clone(),
        controller_binding: controller,
        spawn_operation_id: member.spawn_operation_id,
        spawned_by_turn_id: c
            .query_row(
                "SELECT spawned_by_turn_id FROM brokered_members WHERE run_id=?1",
                [capture.target.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)?,
        membership_state: member.membership_state,
        actor_residency: member.actor_residency,
        activation_policy: c
            .query_row(
                "SELECT activation_policy FROM brokered_members WHERE run_id=?1",
                [capture.target.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)?,
        requested_access: c
            .query_row(
                "SELECT requested_access FROM brokered_members WHERE run_id=?1",
                [capture.target.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)?,
    };
    Ok(Some(Retained {
        capture,
        accepted,
        member,
    }))
}

pub(crate) fn require_claim(
    c: &Connection,
    retained: &Retained,
    claim: Uuid,
) -> Result<(), MachineError> {
    let valid:bool=c.query_row(
        "SELECT EXISTS(SELECT 1 FROM scheduler_claims WHERE claim_id=?1 AND kind='assignment' AND work_ref=?2
         AND target_run_id=?3 AND scope_kind='orchestrated_session' AND scope_ref=?4 AND phase='dispatching')",
        params![claim.to_string(),retained.capture.task.to_string(),retained.capture.target.to_string(),retained.capture.session.to_string()],
        |r|r.get(0)
    ).map_err(internal)?;
    if !valid {
        return Err(integrity("policy task dispatch claim differs"));
    }
    Ok(())
}

pub(crate) fn for_turn(
    c: &Connection,
    root: &Path,
    session: Uuid,
    member: Uuid,
    turn: &str,
) -> Result<Option<Retained>, MachineError> {
    if !table_present(c, "policy_task_dispatch_captures")? {
        return Ok(None);
    }
    let id:Option<String>=c.query_row(
        "SELECT t.task_id FROM brokered_tasks t JOIN policy_task_dispatch_captures p USING(task_id)
         WHERE t.session_id=?1 AND t.target_run_id=?2 AND t.target_turn_id=?3 AND t.state='running'",
        params![session.to_string(),member.to_string(),turn],|r|r.get(0)
    ).optional().map_err(internal)?;
    id.as_deref()
        .map(parse_uuid)
        .transpose()?
        .map(|id| load(c, root, session, id))
        .transpose()
        .map(Option::flatten)
}

pub(crate) fn turn_request(
    retained: &Retained,
    manifest: &crate::run::RunManifest,
) -> Result<crate::turn::TurnRequest, MachineError> {
    if manifest.run_id != retained.capture.target
        || manifest.agent_configuration != retained.member.agent_configuration
        || manifest.controller != retained.member.controller_binding
    {
        return Err(integrity("policy task runtime capture differs"));
    }
    Ok(crate::turn::TurnRequest {
        write: false,
        idempotency_key: format!("brokered-task:{}", retained.capture.task),
        message: retained.accepted.prompt()?,
        images: Vec::new(),
        model: manifest.model.clone(),
        effort: Some(retained.capture.effort.clone()),
        delivery: crate::turn::DeliveryMode::Submit,
    })
}

/// Positive ledger recovery is shared by the central visit and an authorized
/// source's first tool call. It never dispatches or interprets absence as retry.
pub(crate) fn recover(
    root: &Path,
    session: Uuid,
    target: Uuid,
    current_turn: Option<&str>,
) -> Result<bool, MachineError> {
    let mut store = OrchestrationStore::open(root)?;
    if !table_present(&store.connection, "scheduler_claims")?
        || !table_present(&store.connection, "policy_task_dispatch_captures")?
    {
        return Ok(false);
    }
    let row:Option<(String,String)>=store.connection.query_row(
        "SELECT s.claim_id,s.work_ref FROM scheduler_claims s JOIN policy_task_dispatch_captures p ON p.task_id=s.work_ref
         WHERE s.target_run_id=?1 AND s.scope_kind='orchestrated_session' AND s.scope_ref=?2 AND s.kind='assignment'
         AND s.phase IN ('dispatching','accepted','unknown')",
        params![target.to_string(),session.to_string()],|r|Ok((r.get(0)?,r.get(1)?))
    ).optional().map_err(internal)?;
    let Some((claim, task)) = row else {
        return Ok(false);
    };
    let claim = parse_uuid(&claim)?;
    let task = parse_uuid(&task)?;
    let retained = load(&store.connection, root, session, task)?
        .ok_or_else(|| integrity("policy dispatch capture is absent"))?;
    let snapshot = crate::snapshot::RunSnapshot::load(root, target, 0)?;
    let request = turn_request(&retained, &snapshot.manifest)?;
    let namespace = crate::operation::Namespace::submit(
        &retained.member.controller_binding.identity,
        &snapshot.manifest.workspace_id,
        target,
        &request.idempotency_key,
    )
    .map_err(internal)?;
    let ledger = snapshot.observed_ledger(root)?;
    let Some((accepted, generation)) =
        crate::turn::verified_submit_acceptance(ledger.records(), &namespace, &request)
            .map_err(internal)?
    else {
        return Ok(false);
    };
    if generation > snapshot.projection.run_generation
        || snapshot.projection.thread_id.as_deref() != Some(accepted.thread_id.as_str())
        || current_turn.is_some_and(|turn| turn != accepted.turn_id)
    {
        return Err(integrity("original policy Submit native identity differs"));
    }
    let now = store.now_ms()?;
    let tx = store.transaction()?;
    let phase:Option<(String,Option<String>)>=tx.query_row(
        "SELECT phase,turn_id FROM scheduler_claims WHERE claim_id=?1 AND work_ref=?2 AND target_run_id=?3
         AND scope_kind='orchestrated_session' AND scope_ref=?4 AND kind='assignment'",
        params![claim.to_string(),task.to_string(),target.to_string(),session.to_string()],|r|Ok((r.get(0)?,r.get(1)?))
    ).optional().map_err(internal)?;
    let Some((phase, old_turn)) = phase else {
        return Ok(false);
    };
    if !matches!(phase.as_str(), "dispatching" | "accepted" | "unknown")
        || old_turn.as_deref().is_some_and(|id| id != accepted.turn_id)
    {
        return Err(integrity("policy task original claim differs"));
    }
    let current:(String,Option<String>)=tx.query_row("SELECT state,target_turn_id FROM brokered_tasks WHERE task_id=?1 AND session_id=?2 AND target_run_id=?3",
        params![task.to_string(),session.to_string(),target.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(internal)?;
    if current
        .1
        .as_deref()
        .is_some_and(|turn| turn != accepted.turn_id)
    {
        return Err(integrity("policy task original Turn conflicts"));
    }
    if current.1.is_none() {
        if !matches!(
            current.0.as_str(),
            "accepted" | "dispatching" | "interrupted_unknown"
        ) {
            return Err(integrity("policy task terminal state has no accepted Turn"));
        }
        tx.execute("UPDATE brokered_tasks SET state='running',target_turn_id=?2,safe_error_code=NULL,updated_at_ms=?3
            WHERE task_id=?1 AND target_turn_id IS NULL AND state IN ('accepted','dispatching','interrupted_unknown')",
            params![task.to_string(),accepted.turn_id,now]).map_err(internal)?;
        append_event(
            &tx,
            session,
            "specialist_task_turn_accepted",
            &digest_value(
                &serde_json::json!({"task_id":task,"target_run_id":target,"target_turn_id":accepted.turn_id}),
            )?,
            now,
        )?;
    } else if current.0 == "interrupted_unknown" {
        tx.execute("UPDATE brokered_tasks SET state='running',safe_error_code=NULL,updated_at_ms=?2 WHERE task_id=?1 AND target_turn_id=?3",
            params![task.to_string(),now,accepted.turn_id]).map_err(internal)?;
    }
    crate::assignment_queue::remove(&tx, crate::assignment_queue::Owner::Managed(session), task)?;
    if phase != "accepted" || old_turn.is_none() {
        tx.execute("UPDATE scheduler_claims SET phase='accepted',turn_id=?2,safe_blocker=NULL WHERE claim_id=?1",
            params![claim.to_string(),accepted.turn_id]).map_err(internal)?;
        crate::scheduler::event(
            &tx,
            crate::assignment_queue::Owner::Managed(session),
            "scheduler_original_acceptance_linked",
            &format!("{claim}\0{}", accepted.turn_id),
            now,
        )?;
    }
    crate::storage_observation::commit(tx).map_err(internal)?;
    Ok(true)
}

pub(crate) fn bind_submit(
    root: &Path,
    session: Uuid,
    run: Uuid,
    key: &str,
    message: &str,
    effort: Option<&str>,
    write_or_images: bool,
) -> Result<bool, MachineError> {
    let Some(task) = key
        .strip_prefix("brokered-task:")
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return Ok(false);
    };
    let store = OrchestrationStore::open_observer(root)?;
    let Some(retained) = load(&store.connection, root, session, task)? else {
        return Ok(false);
    };
    if retained.capture.target != run
        || write_or_images
        || message != retained.accepted.prompt()?
        || effort != Some(retained.capture.effort.as_str())
    {
        return Err(integrity("policy Submit differs from its original task"));
    }
    let row:Option<(String,String)>=store.connection.query_row(
        "SELECT t.state,s.phase FROM brokered_tasks t JOIN scheduler_claims s ON s.work_ref=t.task_id
         WHERE t.task_id=?1 AND t.session_id=?2 AND t.target_run_id=?3 AND s.target_run_id=?3
         AND s.scope_kind='orchestrated_session' AND s.scope_ref=?2 AND s.kind='assignment'",
        params![task.to_string(),session.to_string(),run.to_string()],|r|Ok((r.get(0)?,r.get(1)?))
    ).optional().map_err(internal)?;
    let Some((state, phase)) = row else {
        return Err(integrity("policy Submit has no owning claim"));
    };
    if state == "dispatching" && phase == "dispatching" && now_ms()? < retained.capture.deadline_ms
    {
        return Ok(true);
    }
    drop(store);
    // Only an already accepted original Submit may cross this boundary after
    // expiry or an accepted claim. It cannot mint another executor operation.
    if matches!(phase.as_str(), "dispatching" | "accepted" | "unknown")
        && recover(root, session, run, None)?
    {
        Ok(true)
    } else {
        Err(integrity(
            "policy Submit is not a fresh or verified original dispatch",
        ))
    }
}

pub(crate) fn link_response(
    tx: &Transaction<'_>,
    retained: &Retained,
    claim: Uuid,
    turn: &str,
    now: i64,
) -> Result<(), MachineError> {
    let capture = &retained.capture;
    let phase:(String,Option<String>)=tx.query_row(
        "SELECT phase,turn_id FROM scheduler_claims WHERE claim_id=?1 AND work_ref=?2 AND target_run_id=?3
         AND scope_kind='orchestrated_session' AND scope_ref=?4 AND kind='assignment'",
        params![claim.to_string(),capture.task.to_string(),capture.target.to_string(),capture.session.to_string()],
        |r|Ok((r.get(0)?,r.get(1)?))
    ).map_err(internal)?;
    if !matches!(phase.0.as_str(), "dispatching" | "accepted" | "unknown")
        || phase.1.as_deref().is_some_and(|id| id != turn)
    {
        return Err(integrity("policy dispatch response claim differs"));
    }
    let current:(String,Option<String>)=tx.query_row(
        "SELECT state,target_turn_id FROM brokered_tasks WHERE task_id=?1 AND session_id=?2 AND target_run_id=?3",
        params![capture.task.to_string(),capture.session.to_string(),capture.target.to_string()],
        |r|Ok((r.get(0)?,r.get(1)?))
    ).map_err(internal)?;
    if let Some(old) = current.1 {
        if old != turn {
            return Err(integrity("policy task original Turn differs"));
        }
        return Ok(());
    }
    if !matches!(
        current.0.as_str(),
        "accepted" | "dispatching" | "interrupted_unknown"
    ) {
        return Err(integrity("policy task is not reserved"));
    }
    tx.execute("UPDATE brokered_tasks SET state='running',target_turn_id=?2,safe_error_code=NULL,updated_at_ms=?3 WHERE task_id=?1 AND target_turn_id IS NULL",
        params![capture.task.to_string(),turn,now]).map_err(internal)?;
    append_event(
        tx,
        capture.session,
        "specialist_task_turn_accepted",
        &digest_value(
            &serde_json::json!({"task_id":capture.task,"target_run_id":capture.target,"target_turn_id":turn}),
        )?,
        now,
    )
}

pub(crate) fn member_session(root: &Path, run: Uuid) -> Result<Option<Uuid>, MachineError> {
    let manifest = crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, root)
        .load_manifest(run)?;
    if manifest.agent_configuration.schema_version != 3 {
        return Ok(None);
    }
    let Some(binding) = manifest.aggregate_binding.as_ref() else {
        return Ok(None);
    };
    if binding.aggregate_kind != crate::domain::AggregateKind::OrchestratedSession
        || binding.member_kind != crate::run::AggregateMemberKind::Specialist
    {
        return Ok(None);
    }
    let store = OrchestrationStore::open_observer(root)?;
    let session = store.session(binding.aggregate_id)?;
    if !matches!(session.specialist_policy, SessionPolicySnapshot::Caller(_)) {
        return Ok(None);
    }
    let member = store.member(session.session_id, run)?;
    if member.agent_configuration != manifest.agent_configuration
        || member.controller_binding != manifest.controller
        || member.agent_configuration.execution_lane != crate::domain::ExecutionLane::Dedicated
    {
        return Err(integrity("policy member native capture differs"));
    }
    Ok(Some(session.session_id))
}

/// The executing Worker supplies a resolved terminal; no call back into that
/// Worker's control queue is made from its terminal hook.
pub(crate) fn settle_terminal(
    root: &Path,
    run: Uuid,
    terminal: &crate::turn::TerminalTurn,
    result: impl FnOnce(
        &crate::turn::FinalResponse,
        &crate::controller::CredentialCarrier,
    ) -> Result<CompletedTaskOutput, MachineError>,
) -> Result<bool, MachineError> {
    let Some(session) = member_session(root, run)? else {
        return Ok(false);
    };
    let store = OrchestrationStore::open(root)?;
    if !table_present(&store.connection, "policy_task_dispatch_captures")? {
        return Ok(true);
    }
    let task:Option<String>=store.connection.query_row(
        "SELECT t.task_id FROM brokered_tasks t JOIN policy_task_dispatch_captures p USING(task_id)
         WHERE t.session_id=?1 AND t.target_run_id=?2 AND t.state IN ('accepted','dispatching','running','interrupted_unknown')
         ORDER BY t.created_at_ms,t.task_id LIMIT 1",
        params![session.to_string(),run.to_string()],|r|r.get(0)
    ).optional().map_err(internal)?;
    let Some(task) = task else {
        return Ok(true);
    };
    let task = parse_uuid(&task)?;
    let retained = load(&store.connection, root, session, task)?
        .ok_or_else(|| integrity("policy terminal capture absent"))?;
    drop(store);
    if !recover(root, session, run, Some(&terminal.turn_id))? {
        return Ok(true);
    }
    let mut store = OrchestrationStore::open(root)?;
    let current = store.task(task)?;
    if current.target_turn_id.as_deref() != Some(terminal.turn_id.as_str()) {
        return Err(integrity("policy terminal Turn differs"));
    }
    if current.state != "running" {
        return Ok(true);
    }
    if terminal.status == "completed" {
        let credential = store.member_credential(run)?;
        let carrier = credential.controller_carrier()?;
        if let Some(response) = &terminal.final_response {
            let output = result(response, &carrier)?;
            store.publish_task_result(session, task, output)?;
        } else {
            store.fail_result_construction(session, task, "SPECIALIST_RESULT_INVALID")?;
        }
    } else {
        let intent = store.task_control_intent(task)?;
        let (state, code) = match (terminal.status.as_str(), intent.as_deref()) {
            ("interrupted", Some("expiry")) => ("expired", Some("OPERATION_TIMEOUT")),
            ("interrupted", Some("cancel")) => ("cancelled", None),
            ("interrupted", _) => ("failed", Some("SPECIALIST_TURN_INTERRUPTED")),
            ("failed", _) => ("failed", Some("SPECIALIST_TURN_FAILED")),
            _ => ("interrupted_unknown", Some("TARGET_TURN_OUTCOME_UNKNOWN")),
        };
        store.update_task_state_if(
            session,
            task,
            "running",
            state,
            code,
            "specialist_task_observed",
        )?;
    }
    let _ = retained;
    crate::scheduler::notify(root, run);
    Ok(true)
}

pub(crate) fn reconcile(
    store: &mut OrchestrationStore,
    session: Uuid,
    run: Uuid,
    adapter: &mut impl OrchestrationAdapter,
) -> Result<(), MachineError> {
    if !table_present(&store.connection, "policy_task_dispatch_captures")? {
        return Ok(());
    }
    let tasks = {
        let mut s=store.connection.prepare(
            "SELECT t.task_id FROM brokered_tasks t JOIN policy_task_dispatch_captures p USING(task_id)
             WHERE t.session_id=?1 AND t.target_run_id=?2 AND t.state IN ('accepted','queued','claimed','dispatching','running','interrupted_unknown')
             ORDER BY t.created_at_ms,t.task_id").map_err(internal)?;
        s.query_map(params![session.to_string(), run.to_string()], |r| {
            r.get::<_, String>(0)
        })
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?
    };
    for task in tasks {
        let task = parse_uuid(&task)?;
        let retained = load(&store.connection, &store.state_root, session, task)?
            .ok_or_else(|| integrity("policy task capture absent"))?;
        let snapshot = store.task(task)?;
        if snapshot.state == "running" {
            store.reconcile_task_observation(session, &snapshot, adapter)?;
        } else if snapshot.state == "queued" && store.now_ms()? >= retained.capture.deadline_ms {
            // Existing central expire_queued owns this transition and causal mail.
            crate::scheduler::notify(&store.state_root, run);
        }
    }
    Ok(())
}
