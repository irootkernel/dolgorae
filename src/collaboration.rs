//! Authenticated Specialist admission. Scheduling and result delivery have
//! separate owners; this boundary commits durable mail without dispatching it.

use crate::machine::MachineError;
use crate::orchestration::{OrchestrationStore, PrimaryCallContext};
use rusqlite::{OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub(crate) const TOOL_NAME: &str = "dolgorae_collaboration";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootPriority {
    schema: String,
    session_id: Uuid,
    root_task_id: Uuid,
    source_run_id: Uuid,
    member_run_id: Uuid,
    priority: u8,
    accepted_task_sha256: String,
    request_sha256: String,
}

/// Retain scheduling authority with the managed task acceptance, before a
/// proved dispatch removes its transient ordering row. The deferred task FK
/// commits only when the accepting transaction also creates the task itself.
pub(crate) fn capture_root_priority(
    tx: &Transaction<'_>,
    task: &crate::orchestration::SuccessorTask<'_>,
    priority: u8,
) -> Result<(), MachineError> {
    if priority > 2 || task.source_run_id != task.session_id {
        return Err(denied());
    }
    install(tx)?;
    let _ = policy(tx, task.session_id)?;
    let record = RootPriority {
        schema: "dolgorae.collaboration-root-priority/v1".into(),
        session_id: task.session_id,
        root_task_id: task.task_id,
        source_run_id: task.source_run_id,
        member_run_id: task.member_run_id,
        priority,
        accepted_task_sha256: task.accepted_request_sha256.into(),
        request_sha256: task.request_sha256.into(),
    };
    let text = serde_json::to_string(&record).map_err(|_| denied())?;
    let digest = crate::jcs::sha256_hex(text.as_bytes());
    tx.execute("INSERT INTO collaboration_root_priorities(root_task_id,session_id,member_run_id,record_json,record_sha256) VALUES(?1,?2,?3,?4,?5)",params![task.task_id.to_string(),task.session_id.to_string(),task.member_run_id.to_string(),text,digest]).map_err(sql)?;
    crate::orchestration::append_event(
        tx,
        task.session_id,
        "collaboration_root_priority_captured",
        &digest,
        task.now_ms,
    )
}

fn root_priority(
    tx: &Transaction<'_>,
    context: &PrimaryCallContext,
    task: Uuid,
    accepted_sha: &str,
    request_sha: &str,
) -> Result<u8, MachineError> {
    let (text,digest): (String,String) = tx.query_row("SELECT record_json,record_sha256 FROM collaboration_root_priorities WHERE root_task_id=?1 AND session_id=?2 AND member_run_id=?3",params![task.to_string(),context.session_id.to_string(),context.source_run_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(sql)?.ok_or_else(denied)?;
    let record: RootPriority = serde_json::from_str(&text).map_err(|_| denied())?;
    if record.schema != "dolgorae.collaboration-root-priority/v1"
        || record.session_id != context.session_id
        || record.root_task_id != task
        || record.source_run_id != context.session_id
        || record.member_run_id != context.source_run_id
        || record.priority > 2
        || record.accepted_task_sha256 != accepted_sha
        || record.request_sha256 != request_sha
        || crate::jcs::sha256_hex(text.as_bytes()) != digest
    {
        return Err(denied());
    }
    let linked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM orchestration_events WHERE session_id=?1 AND kind='collaboration_root_priority_captured' AND payload_sha256=?2)",params![context.session_id.to_string(),digest],|row|row.get(0)).map_err(sql)?;
    if !linked {
        return Err(denied());
    }
    Ok(record.priority)
}

pub(crate) fn enabled(
    state_root: &Path,
    session_id: Uuid,
    run_id: Uuid,
) -> Result<bool, MachineError> {
    let store = OrchestrationStore::open_observer(state_root)?;
    let session = store.session(session_id)?;
    Ok(members(store.collaboration_connection()?, &session)?
        .iter()
        .any(|member| {
            member.run_id == run_id
                && matches!(member.membership_state.as_str(), "active" | "provisioning")
                && member.role_snapshot.collaboration_source()
                && member.agent_configuration.execution_lane
                    == crate::domain::ExecutionLane::Dedicated
        }))
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Target {
    Run { run_id: Uuid },
    Role { role_ref: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
enum Request {
    #[serde(rename = "submit_consultation")]
    Submit {
        target: Target,
        objective: String,
        context_refs: Vec<Uuid>,
        expected_output: Vec<String>,
        blocking: bool,
        deadline_seconds: u64,
    },
    #[serde(rename = "await_consultations")]
    Await {
        exchange_ids: Vec<Uuid>,
        return_when: String,
        transport_wait_seconds: u64,
    },
    #[serde(rename = "collect_collaboration_results")]
    Collect { limit: u64 },
}

pub(crate) fn tool_spec() -> Value {
    let schema: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-collaboration-tool-v1.schema.json"
    ))
    .expect("checked collaboration schema");
    json!({"type":"function", "name":TOOL_NAME,
        "description":"Submit bounded consultations to provisioned peers in this internal Session.",
        "inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema",
            "$defs":schema["$defs"],"oneOf":[
                {"$ref":"#/$defs/submit_request"},
                {"$ref":"#/$defs/await_request"},
                {"$ref":"#/$defs/collect_request"}]}})
}

fn members(
    connection: &rusqlite::Connection,
    session: &crate::orchestration::OrchestratedSessionSnapshot,
) -> Result<Vec<crate::orchestration::BrokeredMemberSnapshot>, MachineError> {
    let mut members = session.members.clone();
    let mut statement = connection.prepare("SELECT run_id,requested_access,role_snapshot_sha256 FROM brokered_members WHERE session_id=?1 AND activation_policy='successor'").map_err(sql)?;
    let rows = statement
        .query_map([session.session_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(sql)?;
    for row in rows {
        let (run, access, role_digest) = row.map_err(sql)?;
        let run = Uuid::parse_str(&run).map_err(|_| denied())?;
        let member = crate::orchestration::successor_member(connection, session.session_id, run)?
            .ok_or_else(denied)?;
        // Successor membership owns its configuration, while the immutable
        // Session policy supplies lateral-role permission and activation policy.
        // Independently admitted successors may have no lateral Role in this
        // Session. Missing permission disables only the optional peer tool.
        // Keep ordinary validation failures for a registered Role fail-closed.
        let registered = match &session.specialist_policy {
            crate::session_policy::SessionPolicySnapshot::Legacy(policy) => policy
                .roles
                .iter()
                .any(|role| role.role_ref == member.role_ref),
            crate::session_policy::SessionPolicySnapshot::Caller(captures) => captures
                .policy()
                .roles
                .iter()
                .any(|role| role.role_ref == member.role_ref),
        };
        if !registered {
            continue;
        }
        let role = session.specialist_policy.role(&member.role_ref)?;
        let configuration_digest =
            crate::run::agent_configuration_digest(&member.agent_configuration)
                .map_err(|_| denied())?;
        members.push(crate::orchestration::BrokeredMemberSnapshot {
            run_id: run,
            parent_run_id: session.root_run_id,
            role_ref: member.role_ref,
            role_snapshot: role.clone(),
            role_snapshot_sha256: role_digest,
            agent_configuration: member.agent_configuration,
            agent_configuration_sha256: configuration_digest,
            controller_binding: serde_json::from_str(
                &connection
                    .query_row(
                        "SELECT controller_binding_json FROM brokered_members WHERE run_id=?1",
                        [run.to_string()],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(sql)?,
            )
            .map_err(|_| denied())?,
            spawn_operation_id: member.spawn_operation_id,
            spawned_by_turn_id: None,
            membership_state: member.membership_state,
            actor_residency: member.actor_residency,
            activation_policy: role.activation_policy().to_owned(),
            requested_access: access,
        });
    }
    Ok(members)
}

fn failure(code: &str) -> MachineError {
    MachineError::new(
        code,
        "collaboration request was not admitted",
        false,
        json!({}),
    )
}
fn denied() -> MachineError {
    failure("COLLABORATION_NOT_ALLOWED")
}
fn sql(error: rusqlite::Error) -> MachineError {
    MachineError::new("INTERNAL_ERROR", error.to_string(), true, json!({}))
}

pub(crate) fn error_result(error: &MachineError) -> Value {
    let (code, retryable, message) = match error.code.as_str() {
        "IDEMPOTENCY_CONFLICT" | "COLLABORATION_IDEMPOTENCY_CONFLICT" => (
            "COLLABORATION_IDEMPOTENCY_CONFLICT",
            false,
            "This consultation call was already accepted with different input.",
        ),
        "COLLABORATION_MAILBOX_FULL" => (
            "COLLABORATION_MAILBOX_FULL",
            error.retryable,
            if error.retryable {
                "The pending consultation queue is full; retry after capacity is available."
            } else {
                "The root task has reached its total consultation limit."
            },
        ),
        "SPECIALIST_ROLE_UNAVAILABLE" => (
            "SPECIALIST_ROLE_UNAVAILABLE",
            false,
            "No eligible provisioned Specialist matches this consultation target.",
        ),
        "TARGET_RUN_NOT_ACTIVE" => (
            "TARGET_RUN_NOT_ACTIVE",
            false,
            "The selected Specialist cannot currently receive this consultation.",
        ),
        "COLLABORATION_TRANSPORT_UNAVAILABLE" => (
            "COLLABORATION_TRANSPORT_UNAVAILABLE",
            error.retryable,
            "This consultation operation is unavailable in this runtime.",
        ),
        "INVALID_ARGUMENT" => (
            "COLLABORATION_NOT_ALLOWED",
            false,
            "The consultation input does not match the checked request contract.",
        ),
        "COLLABORATION_NOT_ALLOWED" => (
            "COLLABORATION_NOT_ALLOWED",
            false,
            "The consultation is not allowed by the current Session authority or request bounds.",
        ),
        _ => (
            "COLLABORATION_TRANSPORT_UNAVAILABLE",
            true,
            "The consultation service could not confirm a safe outcome.",
        ),
    };
    json!({"operation":"collaboration_error","code":code,"retryable":retryable,
        "message":message})
}

/// Only the owned Worker calls this after native Thread/Turn routing. Recheck
/// its current durable generation and member binding before opening a writer.
pub(crate) fn dispatch(
    state_root: &Path,
    call: &crate::turn::PrimaryToolCall,
    worker_run: Uuid,
    generation: u64,
    workspace: &str,
) -> Result<Value, MachineError> {
    let runs = crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, state_root);
    let manifest = runs.load_manifest(worker_run)?;
    let projection = runs.load_state_projection(worker_run)?;
    let binding = manifest.aggregate_binding.as_ref().ok_or_else(denied)?;
    if call.tool_name != TOOL_NAME
        || generation == 0
        || call.run_generation != generation
        || projection.run_generation != generation
        || projection.thread_id.as_deref() != Some(call.source_thread_id.as_str())
        || projection.active_turn_id.as_deref() != Some(call.context.source_turn_id.as_str())
        || call.context.source_run_id != worker_run
        || manifest.workspace_id != workspace
        || binding.aggregate_kind != crate::domain::AggregateKind::OrchestratedSession
        || binding.member_kind != crate::run::AggregateMemberKind::Specialist
        || binding.aggregate_id != call.context.session_id
    {
        return Err(denied());
    }
    let mut store = OrchestrationStore::open(state_root)?;
    let mut effects = crate::semantic::ProductionOrchestrationEffects::new(state_root);
    admit(&mut store, &call.context, &call.payload, &mut effects)
}

/// Semantic boundary used by the native dispatcher and storage tests. This is
/// crate-private and cannot expose a CLI/model-selected source identity.
pub(crate) fn admit(
    store: &mut OrchestrationStore,
    context: &PrimaryCallContext,
    payload: &Value,
    adapter: &mut impl crate::orchestration::OrchestrationAdapter,
) -> Result<Value, MachineError> {
    if payload.to_string().len() > 1_048_576 {
        return Err(denied());
    }
    let state_root = store.collaboration_state_root().to_path_buf();
    let request: Request = serde_json::from_value(payload.clone()).map_err(|_| denied())?;
    let session = store.session(context.session_id)?;
    if session.status != "active" || session.root_run_id == context.source_run_id {
        return Err(denied());
    }
    let members = members(store.collaboration_connection()?, &session)?;
    let source = members
        .iter()
        .find(|member| member.run_id == context.source_run_id)
        .filter(|member| {
            member.membership_state == "active"
                && member.actor_residency == "resident"
                && member.role_snapshot.collaboration_source()
        })
        .ok_or_else(denied)?;
    if source.agent_configuration.execution_lane != crate::domain::ExecutionLane::Dedicated {
        return Err(denied());
    }
    let runs = crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, &state_root);
    let root = runs.load_state_projection(session.root_run_id)?;
    let source_core = runs.load_state_projection(source.run_id)?;
    if !matches!(
        root.lifecycle,
        crate::domain::RunLifecycle::Idle
            | crate::domain::RunLifecycle::Running
            | crate::domain::RunLifecycle::WaitingInteraction
    ) || source_core.active_turn_id.as_deref() != Some(context.source_turn_id.as_str())
        || !matches!(
            source_core.lifecycle,
            crate::domain::RunLifecycle::Running | crate::domain::RunLifecycle::WaitingInteraction
        )
    {
        return Err(denied());
    }
    for text in [&context.source_turn_id, &context.source_tool_call_id] {
        if text.is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
            return Err(denied());
        }
    }
    match request {
        Request::Await {
            exchange_ids,
            return_when,
            transport_wait_seconds,
        } => {
            if exchange_ids.is_empty()
                || exchange_ids.len() > 16
                || exchange_ids
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != exchange_ids.len()
                || !matches!(return_when.as_str(), "any" | "all")
                || !(1..=60).contains(&transport_wait_seconds)
            {
                return Err(denied());
            }
            // TASK-083 owns wait-edge and outbox semantics. Do not manufacture
            // an empty success or perform a blocking wait before that boundary.
            Err(failure("COLLABORATION_TRANSPORT_UNAVAILABLE"))
        }
        Request::Collect { limit } => {
            if !(1..=64).contains(&limit) {
                return Err(denied());
            }
            Err(failure("COLLABORATION_TRANSPORT_UNAVAILABLE"))
        }
        Request::Submit {
            target,
            objective,
            context_refs,
            expected_output,
            blocking,
            deadline_seconds,
        } => {
            if objective.is_empty()
                || objective.len() > 65_536
                || objective.contains('\0')
                || context_refs.len() > 64
                || context_refs.iter().any(|id| id.get_version_num() != 7)
                || context_refs
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != context_refs.len()
                || expected_output.is_empty()
                || expected_output.len() > 32
                || expected_output
                    .iter()
                    .any(|text| text.is_empty() || text.len() > 512 || text.contains('\0'))
                || !(1..=3600).contains(&deadline_seconds)
            {
                return Err(denied());
            }
            // Blocking admission requires TASK-083's writer/wait-graph checks.
            if blocking {
                return Err(failure("COLLABORATION_TRANSPORT_UNAVAILABLE"));
            }
            let tx = store.collaboration_transaction()?;
            let current_session: (String, String) = tx
                .query_row(
                    "SELECT status,policy_sha256 FROM orchestrated_sessions WHERE session_id=?1",
                    [context.session_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(sql)?;
            let current_source: (String,String,String) = tx.query_row(
                "SELECT membership_state,actor_residency,agent_configuration_sha256 FROM brokered_members WHERE session_id=?1 AND run_id=?2",
                params![context.session_id.to_string(),source.run_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))
            ).map_err(sql)?;
            if current_session != ("active".into(), session.specialist_policy_sha256.clone())
                || current_source
                    != (
                        "active".into(),
                        "resident".into(),
                        source.agent_configuration_sha256.clone(),
                    )
            {
                return Err(denied());
            }
            let body = crate::jcs::canonicalize(
                &crate::jcs::parse(&payload.to_string()).map_err(|_| denied())?,
            )
            .map_err(|_| denied())?;
            let body_digest = crate::jcs::sha256_hex(&body);
            let key = crate::jcs::sha256_hex(
                format!(
                    "dolgorae-collaboration-v1\0{}\0{}\0{}\0{}",
                    context.session_id,
                    context.source_run_id,
                    context.source_turn_id,
                    context.source_tool_call_id
                )
                .as_bytes(),
            );
            let (task, accepted) = crate::orchestration::successor_task_for_turn(
                &tx,
                context.session_id,
                context.source_run_id,
                &context.source_turn_id,
            )?
            .ok_or_else(denied)?;
            let accepted_sha = crate::jcs::sha256_hex(accepted.as_bytes());
            let accepted: crate::delegation_identity::AcceptedTask =
                serde_json::from_str(&accepted).map_err(|_| denied())?;
            accepted.digest()?;
            let accepted_json_digest: String = tx.query_row("SELECT accepted_request_sha256 FROM brokered_task_acceptances WHERE task_id=?1",[task.to_string()],|row|row.get(0)).map_err(sql)?;
            if accepted_sha != accepted_json_digest {
                return Err(denied());
            }
            let (task_body, contexts, retained_target) =
                crate::engagement::accepted_task_content(&tx, task)?.ok_or_else(denied)?;
            let original: crate::task_request::SpecialistTaskRequest =
                serde_json::from_slice(&task_body).map_err(|_| denied())?;
            let facts = crate::orchestration::successor_task(&tx, context.session_id, task)?
                .ok_or_else(denied)?;
            let marker: Value = serde_json::from_str(&facts.request_json).map_err(|_| denied())?;
            let root_deadline = marker["deadline_seconds"].as_u64().ok_or_else(denied)?;
            if !crate::successor_capture::verify_accepted_assignment(
                source.run_id,
                &original,
                &accepted,
                root_deadline,
                &format!("sha256:{}", facts.source_request_sha256),
            )? {
                return Err(denied());
            }
            crate::successor_capture::AcceptedTaskContents {
                task_body,
                contexts,
                target: retained_target,
            }
            .prompt(&accepted, &accepted.execution_intent, 1)?;
            let expected_role = source
                .agent_configuration
                .instructions
                .delegation()
                .map_or_else(
                    || format!("sha256:{}", source.role_snapshot_sha256),
                    |instructions| instructions.role_snapshot_digest.clone(),
                );
            let expected_configuration = source
                .agent_configuration
                .admitted_configuration
                .as_ref()
                .map_or_else(
                    || Ok(format!("sha256:{}", source.agent_configuration_sha256)),
                    |configuration| configuration.digest(),
                )?;
            if accepted.role_snapshot_digest != expected_role
                || accepted.agent_configuration_digest != expected_configuration
                || expected_role != format!("sha256:{}", source.role_snapshot_sha256)
            {
                return Err(denied());
            }
            let priority = root_priority(
                &tx,
                context,
                task,
                &accepted_sha,
                &facts.source_request_sha256,
            )?;
            let table_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='collaboration_exchanges')", [], |row| row.get(0)).map_err(sql)?;
            if table_exists {
                let _ = policy(&tx, context.session_id)?;
                let replay: Option<(String,String,String)> = tx.query_row("SELECT request_sha256,exchange_id,target_run_id FROM collaboration_exchanges WHERE session_id=?1 AND source_run_id=?2 AND source_turn_id=?3 AND source_call_id=?4",params![context.session_id.to_string(),context.source_run_id.to_string(),context.source_turn_id,context.source_tool_call_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(sql)?;
                if let Some((digest, exchange, target)) = replay {
                    if digest != body_digest {
                        return Err(failure("COLLABORATION_IDEMPOTENCY_CONFLICT"));
                    }
                    return Ok(
                        json!({"operation":"submit_consultation_result","exchange_id":exchange,"target_run_id":target,"state":"queued"}),
                    );
                }
            }
            let policy = policy(&tx, context.session_id)?;
            if payload.to_string().len() as u64
                > policy["max_inline_request_bytes"]
                    .as_u64()
                    .ok_or_else(denied)?
            {
                return Err(denied());
            }
            let candidates = members.iter().filter(|member| {
                member.run_id != source.run_id
                    && member.membership_state == "active"
                    && matches!(member.actor_residency.as_str(), "resident" | "passivated")
                    && member.role_snapshot.collaboration_target()
                    && member.requested_access == "read_only"
                    && member.agent_configuration.execution_lane
                        == crate::domain::ExecutionLane::Dedicated
                    && match &target {
                        Target::Run { run_id } => {
                            member.run_id == *run_id && run_id.get_version_num() == 7
                        }
                        Target::Role { role_ref } => role_ref == &member.role_ref,
                    }
            });
            let runs =
                crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, &state_root);
            let mut eligible = Vec::new();
            for candidate in candidates {
                let core = runs.load_state_projection(candidate.run_id)?;
                if !matches!(
                    core.lifecycle,
                    crate::domain::RunLifecycle::Idle
                        | crate::domain::RunLifecycle::Running
                        | crate::domain::RunLifecycle::WaitingInteraction
                ) {
                    continue;
                }
                let pending = if table_exists {
                    tx.query_row("SELECT count(*) FROM collaboration_exchanges WHERE target_run_id=?1 AND execution_state IN ('queued','claimed','dispatching','running')",[candidate.run_id.to_string()],|row|row.get::<_,u64>(0)).map_err(sql)?
                } else {
                    0
                };
                eligible.push((
                    core.lifecycle != crate::domain::RunLifecycle::Idle,
                    pending,
                    candidate.run_id,
                    candidate,
                ));
            }
            let target = eligible
                .into_iter()
                .min_by_key(|entry| (entry.0, entry.1, entry.2))
                .map(|entry| entry.3)
                .ok_or_else(|| failure("SPECIALIST_ROLE_UNAVAILABLE"))?;
            if target.actor_residency == "passivated" && target.activation_policy != "on_mail" {
                return Err(failure("TARGET_RUN_NOT_ACTIVE"));
            }
            let contexts = adapter.resolve_task_contexts(context.source_run_id, &context_refs)?;
            let context_json = serde_json::to_string(&contexts).map_err(|_| denied())?;
            let current_target: (String,String,String) = tx.query_row(
                "SELECT membership_state,actor_residency,agent_configuration_sha256 FROM brokered_members WHERE session_id=?1 AND run_id=?2",
                params![context.session_id.to_string(),target.run_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))
            ).map_err(sql)?;
            if current_target
                != (
                    "active".into(),
                    target.actor_residency.clone(),
                    target.agent_configuration_sha256.clone(),
                )
            {
                return Err(denied());
            }
            install(&tx)?;
            let pending: (u64,u64,u64,u64) = tx.query_row("SELECT count(*),coalesce(sum(target_run_id=?2),0),coalesce(sum(target_run_id=?2 AND source_run_id=?3),0),(SELECT count(*) FROM collaboration_exchanges WHERE session_id=?1 AND root_task_id=?4) FROM collaboration_exchanges WHERE session_id=?1 AND execution_state IN ('queued','claimed','dispatching','running')",params![context.session_id.to_string(),target.run_id.to_string(),source.run_id.to_string(),task.to_string()], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).map_err(sql)?;
            if pending.3
                >= policy["max_exchanges_per_root_task"]
                    .as_u64()
                    .ok_or_else(denied)?
            {
                return Err(failure("COLLABORATION_MAILBOX_FULL"));
            }
            if pending.0
                >= policy["max_pending_per_session"]
                    .as_u64()
                    .ok_or_else(denied)?
                || pending.1
                    >= policy["max_pending_per_specialist"]
                        .as_u64()
                        .ok_or_else(denied)?
                || pending.2
                    >= policy["max_pending_per_source_target"]
                        .as_u64()
                        .ok_or_else(denied)?
            {
                return Err(MachineError::new(
                    "COLLABORATION_MAILBOX_FULL",
                    "pending consultation capacity is full",
                    true,
                    json!({}),
                ));
            }
            let exchange = crate::machine::new_uuid_v7();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| denied())?
                .as_millis();
            let now = i64::try_from(now).map_err(|_| denied())?;
            let deadline = now
                .checked_add(i64::try_from(deadline_seconds).map_err(|_| denied())? * 1000)
                .ok_or_else(denied)?;
            let admission_digest = crate::jcs::sha256_hex(json!({
                "exchange_id":exchange,"session_id":context.session_id,
                "source_run_id":source.run_id,"source_turn_id":context.source_turn_id,
                "source_call_id":context.source_tool_call_id,"source_task_id":task,"root_task_id":task,
                "target_run_id":target.run_id,"request_sha256":body_digest,
                "context_sha256":crate::jcs::sha256_hex(context_json.as_bytes()),
                "root_priority":priority,"deadline_ms":deadline
            }).to_string().as_bytes());
            tx.execute("INSERT INTO collaboration_exchanges(exchange_id,session_id,source_run_id,source_turn_id,source_call_id,idempotency_key,source_task_id,root_task_id,target_run_id,request_json,request_sha256,context_json,root_priority,deadline_ms,execution_state,delivery_state,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,?8,?9,?10,?11,?12,?13,'queued','none',?14)",params![exchange.to_string(),context.session_id.to_string(),source.run_id.to_string(),context.source_turn_id,context.source_tool_call_id,key,task.to_string(),target.run_id.to_string(),String::from_utf8(body).map_err(|_| denied())?,body_digest,context_json,priority,deadline,now]).map_err(sql)?;
            tx.execute(
                "INSERT INTO collaboration_mailbox(exchange_id,target_run_id) VALUES(?1,?2)",
                params![exchange.to_string(), target.run_id.to_string()],
            )
            .map_err(sql)?;
            tx.execute("INSERT INTO collaboration_activation_requests(target_run_id,session_id,requested_at_ms) VALUES(?1,?2,?3) ON CONFLICT(target_run_id) DO NOTHING",params![target.run_id.to_string(),context.session_id.to_string(),now]).map_err(sql)?;
            crate::orchestration::append_event(
                &tx,
                context.session_id,
                "collaboration_queued",
                &admission_digest,
                now,
            )?;
            crate::storage_observation::commit(tx).map_err(sql)?;
            Ok(
                json!({"operation":"submit_consultation_result","exchange_id":exchange,"target_run_id":target.run_id,"state":"queued"}),
            )
        }
    }
}

fn policy(tx: &Transaction<'_>, session: Uuid) -> Result<Value, MachineError> {
    // The runtime has no collaboration policy override operation yet. Capture
    // the SPEC-012 v1 defaults once; later consumers read the retained snapshot.
    let defaults = json!({
        "priority_classes":{"interactive":3,"normal":2,"background":1},
        "starvation_limit_seconds":300,"max_consecutive_from_same_source":2,
        "max_pending_per_specialist":16,"max_pending_per_source_target":4,
        "max_pending_per_session":128,"max_blocking_waits_per_run":2,
        "max_blocking_depth":2,"max_exchanges_per_root_task":8,
        "max_inline_request_bytes":65536,"idle_passivation_seconds":600,
        "activation_timeout_seconds":30,"max_activation_attempts":3,
        "reconciliation_interval_seconds":30
    });
    let text = defaults.to_string();
    let digest = crate::jcs::sha256_hex(text.as_bytes());
    let retained: Option<(String, String)> = tx
        .query_row(
            "SELECT policy_json,policy_sha256 FROM collaboration_policies WHERE session_id=?1",
            [session.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    if let Some((retained, retained_digest)) = retained {
        if retained != text || retained_digest != digest {
            return Err(denied());
        }
    } else {
        tx.execute("INSERT INTO collaboration_policies(session_id,policy_json,policy_sha256) VALUES(?1,?2,?3)",params![session.to_string(),text,digest]).map_err(sql)?;
    }
    Ok(defaults)
}

pub(crate) fn validate_schema(connection: &rusqlite::Connection) -> Result<(), MachineError> {
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key='collaboration_schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    let objects: u64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE (type='table' AND name IN ('collaboration_root_priorities','collaboration_policies','collaboration_exchanges','collaboration_mailbox','collaboration_activation_requests')) OR (type='index' AND name='collaboration_mailbox_target')",[],|row|row.get(0)).map_err(sql)?;
    if !matches!((version.as_deref(), objects), (None, 0) | (Some("1"), 6)) {
        return Err(failure("ORCHESTRATION_SCHEMA_UNSUPPORTED"));
    }
    if objects != 0 {
        for query in [
            "SELECT root_task_id,session_id,member_run_id,record_json,record_sha256 FROM collaboration_root_priorities LIMIT 0",
            "SELECT session_id,policy_json,policy_sha256 FROM collaboration_policies LIMIT 0",
            "SELECT exchange_id,session_id,source_run_id,source_turn_id,source_call_id,idempotency_key,source_task_id,root_task_id,target_run_id,request_json,request_sha256,context_json,root_priority,deadline_ms,execution_state,delivery_state,created_at_ms FROM collaboration_exchanges LIMIT 0",
            "SELECT sequence,exchange_id,target_run_id FROM collaboration_mailbox LIMIT 0",
            "SELECT target_run_id,session_id,requested_at_ms FROM collaboration_activation_requests LIMIT 0",
        ] {
            connection.prepare(query).map_err(sql)?;
        }
    }
    Ok(())
}

fn install(tx: &Transaction<'_>) -> Result<(), MachineError> {
    let version: Option<String> = tx
        .query_row(
            "SELECT value FROM metadata WHERE key='collaboration_schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    match version.as_deref() {
        Some("1") => return Ok(()),
        Some(_) => return Err(denied()),
        None => {}
    }
    tx.execute_batch("CREATE TABLE collaboration_root_priorities(root_task_id TEXT PRIMARY KEY REFERENCES brokered_tasks(task_id) DEFERRABLE INITIALLY DEFERRED,session_id TEXT NOT NULL REFERENCES orchestrated_sessions(session_id),member_run_id TEXT NOT NULL REFERENCES brokered_members(run_id),record_json TEXT NOT NULL,record_sha256 TEXT NOT NULL) STRICT;
        CREATE TABLE collaboration_policies(session_id TEXT PRIMARY KEY REFERENCES orchestrated_sessions(session_id),policy_json TEXT NOT NULL,policy_sha256 TEXT NOT NULL) STRICT;
        CREATE TABLE collaboration_exchanges(
        exchange_id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES orchestrated_sessions(session_id),
        source_run_id TEXT NOT NULL REFERENCES brokered_members(run_id),source_turn_id TEXT NOT NULL,source_call_id TEXT NOT NULL,
        idempotency_key TEXT NOT NULL,source_task_id TEXT NOT NULL REFERENCES brokered_tasks(task_id),root_task_id TEXT NOT NULL REFERENCES brokered_tasks(task_id),
        target_run_id TEXT NOT NULL REFERENCES brokered_members(run_id),request_json TEXT NOT NULL,request_sha256 TEXT NOT NULL,context_json TEXT NOT NULL,
        root_priority INTEGER NOT NULL CHECK(root_priority BETWEEN 0 AND 2),deadline_ms INTEGER NOT NULL,
        execution_state TEXT NOT NULL,delivery_state TEXT NOT NULL,created_at_ms INTEGER NOT NULL,
        UNIQUE(session_id,source_run_id,source_turn_id,source_call_id)) STRICT;
        CREATE TABLE collaboration_mailbox(sequence INTEGER PRIMARY KEY AUTOINCREMENT,exchange_id TEXT NOT NULL UNIQUE REFERENCES collaboration_exchanges(exchange_id),target_run_id TEXT NOT NULL REFERENCES brokered_members(run_id)) STRICT;
        CREATE INDEX collaboration_mailbox_target ON collaboration_mailbox(target_run_id,sequence);
        CREATE TABLE collaboration_activation_requests(target_run_id TEXT PRIMARY KEY REFERENCES brokered_members(run_id),session_id TEXT NOT NULL REFERENCES orchestrated_sessions(session_id),requested_at_ms INTEGER NOT NULL) STRICT;
        INSERT INTO metadata(key,value) VALUES('collaboration_schema_version','1');").map_err(sql)
}
