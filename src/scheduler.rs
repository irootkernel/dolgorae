//! One gateway-owned scheduler. SQLite owns work, claims and actor residency;
//! the socket and dirty set carry reconstructable hints only.

use crate::assignment_queue::Owner;
use crate::machine::MachineError;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) fn error(reason: &str) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "scheduler requires reconciliation",
        false,
        serde_json::json!({"reason":reason}),
    )
}
pub(crate) fn sql(_: rusqlite::Error) -> MachineError {
    error("scheduler_storage_unavailable")
}

pub(crate) fn now_ms() -> Result<i64, MachineError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|n| i64::try_from(n.as_millis()).ok())
        .ok_or_else(|| error("clock_unavailable"))
}

pub(crate) fn install(tx: &Transaction<'_>) -> Result<(), MachineError> {
    let version: Option<String> = tx
        .query_row(
            "SELECT value FROM metadata WHERE key='actor_scheduler_schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    match version.as_deref() {
        Some("1") => return Ok(()),
        Some(_) => return Err(error("unsupported_scheduler_schema")),
        None => {}
    }
    tx.execute_batch("CREATE TABLE scheduler_claims(
        claim_id TEXT PRIMARY KEY,kind TEXT NOT NULL,work_ref TEXT NOT NULL UNIQUE,
        target_run_id TEXT NOT NULL UNIQUE,scope_kind TEXT NOT NULL,scope_ref TEXT NOT NULL,
        lease_until_ms INTEGER NOT NULL,phase TEXT NOT NULL,
        turn_id TEXT, safe_blocker TEXT) STRICT;
        CREATE TABLE scheduler_fairness(target_run_id TEXT PRIMARY KEY,source_ref TEXT NOT NULL,
        effective_priority INTEGER NOT NULL,consecutive INTEGER NOT NULL) STRICT;
        CREATE TABLE scheduler_actors(target_run_id TEXT PRIMARY KEY,scope_kind TEXT NOT NULL,
        scope_ref TEXT NOT NULL,observed_revision INTEGER NOT NULL,idle_since_ms INTEGER NOT NULL,
        operation_id TEXT,operation_kind TEXT,lease_until_ms INTEGER,attempts INTEGER NOT NULL DEFAULT 0,
        safe_blocker TEXT,operation_snapshot_json TEXT) STRICT;
        CREATE TABLE scheduler_external_policies(scope_ref TEXT PRIMARY KEY,policy_json TEXT NOT NULL,policy_sha256 TEXT NOT NULL) STRICT;
        CREATE TABLE scheduler_operations(operation_id TEXT PRIMARY KEY,target_run_id TEXT NOT NULL,
        scope_kind TEXT NOT NULL,scope_ref TEXT NOT NULL,kind TEXT NOT NULL,
        lease_until_ms INTEGER NOT NULL,attempt INTEGER NOT NULL,identity_json TEXT NOT NULL,
        identity_sha256 TEXT NOT NULL,state TEXT NOT NULL,safe_blocker TEXT,observed_identity_json TEXT) STRICT;
        INSERT INTO metadata(key,value) VALUES('actor_scheduler_schema_version','1');").map_err(sql)
}

pub(crate) fn validate_schema(c: &Connection) -> Result<(), MachineError> {
    let version: Option<String> = c
        .query_row(
            "SELECT value FROM metadata WHERE key='actor_scheduler_schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    let objects: u32 = c.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('scheduler_claims','scheduler_fairness','scheduler_actors','scheduler_external_policies','scheduler_operations')",[],|r|r.get(0)).map_err(sql)?;
    if !matches!((version.as_deref(), objects), (None, 0) | (Some("1"), 5)) {
        return Err(error("unsupported_scheduler_schema"));
    }
    if objects != 0 {
        c.prepare("SELECT claim_id,kind,work_ref,target_run_id,scope_kind,scope_ref,lease_until_ms,phase,turn_id,safe_blocker FROM scheduler_claims LIMIT 0").map_err(sql)?;
        c.prepare("SELECT target_run_id,source_ref,effective_priority,consecutive FROM scheduler_fairness LIMIT 0").map_err(sql)?;
        c.prepare("SELECT target_run_id,scope_kind,scope_ref,observed_revision,idle_since_ms,operation_id,operation_kind,lease_until_ms,attempts,safe_blocker,operation_snapshot_json FROM scheduler_actors LIMIT 0").map_err(sql)?;
        c.prepare(
            "SELECT scope_ref,policy_json,policy_sha256 FROM scheduler_external_policies LIMIT 0",
        )
        .map_err(sql)?;
        c.prepare("SELECT operation_id,target_run_id,scope_kind,scope_ref,kind,lease_until_ms,attempt,identity_json,identity_sha256,state,safe_blocker,observed_identity_json FROM scheduler_operations LIMIT 0").map_err(sql)?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub kind: String,
    pub id: Uuid,
    pub owner: Owner,
    pub target: Uuid,
    pub source: String,
    pub priority: u8,
    pub deadline_ms: i64,
    pub created_ms: i64,
    pub sequence: i64,
    pub dependency_unblock: bool,
}

pub(crate) fn select(
    mut candidates: Vec<Candidate>,
    now: i64,
    history: Option<(&str, u8, u32)>,
    policy: Policy,
) -> Option<Candidate> {
    let key = |c: &Candidate| {
        (
            now.saturating_sub(c.created_ms) < policy.starvation_ms,
            c.priority,
            !c.dependency_unblock,
            c.deadline_ms,
            c.sequence,
        )
    };
    candidates.sort_by_key(key);
    let first = candidates.first()?;
    if let Some((source, _, count)) = history
        && count >= policy.consecutive
        && first.source == source
    {
        let starved = now.saturating_sub(first.created_ms) >= policy.starvation_ms;
        if let Some(index) = candidates.iter().position(|c| {
            c.source != source
                && c.priority == first.priority
                && (now.saturating_sub(c.created_ms) >= policy.starvation_ms) == starved
        }) {
            return Some(candidates.remove(index));
        }
    }
    Some(candidates.remove(0))
}

fn table(c: &Connection, name: &str) -> Result<bool, MachineError> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .map_err(sql)
}

pub(crate) fn candidates(
    c: &Connection,
    owner: Owner,
    target: Uuid,
) -> Result<Vec<Candidate>, MachineError> {
    let (scope_kind, scope) = owner.scope();
    let mut result = Vec::new();
    if table(c, "assignment_queue_entries")? {
        let query = match owner {
            Owner::Managed(_) => {
                "SELECT q.task_id,q.source_ref,q.priority,q.deadline_ms,t.created_at_ms,q.sequence FROM assignment_queue_entries q JOIN brokered_tasks t ON t.task_id=q.task_id WHERE q.scope_kind=?1 AND q.scope_ref=?2 AND q.target_run_id=?3 AND t.state='queued'"
            }
            Owner::External(_) => {
                "SELECT q.task_id,q.source_ref,q.priority,q.deadline_ms,t.created_at_ms,q.sequence FROM assignment_queue_entries q JOIN tasks t ON t.task_id=q.task_id WHERE q.scope_kind=?1 AND q.scope_ref=?2 AND q.target_run_id=?3 AND t.state='queued'"
            }
        };
        let mut s = c.prepare(query).map_err(sql)?;
        for row in s
            .query_map(
                params![scope_kind, scope.to_string(), target.to_string()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, u8>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                },
            )
            .map_err(sql)?
        {
            let (id, source, priority, deadline_ms, created_ms, sequence) = row.map_err(sql)?;
            result.push(Candidate {
                kind: "assignment".into(),
                id: Uuid::parse_str(&id).map_err(|_| error("invalid_task_identity"))?,
                owner,
                target,
                source,
                priority,
                deadline_ms,
                created_ms,
                sequence,
                dependency_unblock: false,
            });
        }
    }
    if let Owner::Managed(_) = owner
        && table(c, "collaboration_exchanges")?
    {
        let mut s=c.prepare("SELECT e.exchange_id,e.source_run_id,e.root_priority,e.deadline_ms,e.created_at_ms,m.sequence FROM collaboration_exchanges e JOIN collaboration_mailbox m USING(exchange_id) WHERE e.session_id=?1 AND e.target_run_id=?2 AND e.execution_state='queued'").map_err(sql)?;
        for row in s
            .query_map(params![scope.to_string(), target.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u8>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(sql)?
        {
            let (id, source, priority, deadline_ms, created_ms, sequence) = row.map_err(sql)?;
            result.push(Candidate {
                kind: "consultation".into(),
                id: Uuid::parse_str(&id).map_err(|_| error("invalid_exchange_identity"))?,
                owner,
                target,
                source,
                priority,
                deadline_ms,
                created_ms,
                sequence,
                dependency_unblock: false,
            });
        }
    }
    // Both managed streams already commit into one causal event chain. Reuse
    // that sequence, including historical same-millisecond ties, rather than
    // inventing a new acceptance order during gateway startup.
    for candidate in &mut result {
        if candidate.kind == "assignment" {
            if candidate.source != scope.to_string() {
                return Err(error("queued_source_changed"));
            }
            if let Owner::Managed(session) = owner
                && let Some(priority) =
                    crate::collaboration::queued_root_priority(c, session, candidate.id, target)?
                && priority != candidate.priority
            {
                return Err(error("queued_root_priority_changed"));
            }
        }
        let digest = if candidate.kind == "assignment" {
            crate::jcs::sha256_hex(candidate.id.as_bytes())
        } else {
            crate::collaboration::execution_record(c, candidate.id)?.event_digest
        };
        let (query, scope) = match owner {
            Owner::Managed(scope) => (
                "SELECT COUNT(*),MIN(sequence) FROM orchestration_events WHERE session_id=?1 AND kind=?2 AND payload_sha256=?3",
                scope,
            ),
            Owner::External(scope) => (
                "SELECT COUNT(*),MIN(sequence) FROM events WHERE engagement_id=?1 AND kind=?2 AND payload_sha256=?3",
                scope,
            ),
        };
        let digest = if matches!(owner, Owner::External(_)) {
            crate::jcs::sha256_hex(digest.as_bytes())
        } else {
            digest
        };
        let (count, sequence): (u64, Option<i64>) = c
            .query_row(
                query,
                params![
                    scope.to_string(),
                    if candidate.kind == "assignment" {
                        "assignment_queued"
                    } else {
                        "collaboration_queued"
                    },
                    digest
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sql)?;
        if count != 1 {
            return Err(error("queued_causal_event_missing"));
        }
        candidate.sequence = sequence.ok_or_else(|| error("queued_causal_sequence_missing"))?;
    }
    Ok(result)
}

/// A lease reference authorizes only the retained work. It is not a Controller
/// credential, and ordinary Run authorization never accepts its carrier.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AcceptedWork {
    pub schema: String,
    pub claim: Uuid,
    pub work: Uuid,
    pub target: Uuid,
    pub scope_kind: String,
    pub scope_ref: Uuid,
    pub kind: String,
}

pub(crate) struct RetainedTask {
    pub accepted: crate::delegation_identity::AcceptedTask,
    pub contents: crate::successor_capture::AcceptedTaskContents,
    pub key: String,
    pub deadline_seconds: u64,
    pub deadline_ms: i64,
}

pub(crate) fn retained_task(
    store: &crate::engagement::EngagementStore,
    permit: &AcceptedWork,
    workspace: &str,
    state_root: &Path,
) -> Result<RetainedTask, MachineError> {
    if permit.schema != "dolgorae.accepted-work/v1" || permit.kind != "assignment" {
        return Err(error("accepted_work_scope_invalid"));
    }
    let c = store.connection();
    let operation = match permit.scope_kind.as_str() {
        "external_engagement" => "assign_external_specialist_task",
        "orchestrated_session" => "assign_specialist_task",
        _ => return Err(error("accepted_work_owner_invalid")),
    };
    let mut statement=c.prepare("SELECT receipt_json,controller_principal,controller_generation FROM delegation_operations WHERE workspace_ref=?1 AND scope_kind=?2 AND scope_ref=?3 AND operation_kind=?4 AND state='accepted' AND json_extract(accepted_subjects_json,'$.task_ref')=?5").map_err(sql)?;
    let receipts = statement
        .query_map(
            params![
                workspace,
                permit.scope_kind,
                permit.scope_ref.to_string(),
                operation,
                permit.work.to_string()
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                ))
            },
        )
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    let [(text, principal, generation)] = receipts.as_slice() else {
        return Err(error("accepted_receipt_ambiguous"));
    };
    let receipt: crate::delegation_identity::OperationReceipt =
        serde_json::from_str(text).map_err(|_| error("accepted_receipt_invalid"))?;
    receipt.validate()?;
    match store.intent_lookup(&receipt.namespace)? {
        crate::engagement::IntentLookup::Accepted(retained) if *retained == receipt => {}
        _ => return Err(error("accepted_receipt_changed")),
    }
    if receipt.accepted_subjects.task_ref.as_deref() != Some(permit.work.to_string().as_str())
        || receipt.accepted_subjects.member_ref.as_deref()
            != Some(permit.target.to_string().as_str())
        || receipt.namespace.workspace_ref != workspace
        || receipt.namespace.scope_kind.as_deref() != Some(permit.scope_kind.as_str())
        || receipt.namespace.scope_ref.as_deref() != Some(permit.scope_ref.to_string().as_str())
        || receipt.namespace.operation_kind != operation
    {
        return Err(error("accepted_receipt_mismatch"));
    }
    let (accepted, key, deadline_seconds, origin, recorded) = if permit.scope_kind
        == "external_engagement"
    {
        let (_, owner) = store.accepted_external_owner(permit.scope_ref)?;
        let expected = crate::operation::Namespace::delegation(
            &owner.identity,
            workspace,
            Some(("external_engagement", &permit.scope_ref.to_string())),
            operation,
            &receipt.namespace.idempotency_key,
        )
        .map_err(|_| error("accepted_owner_namespace_invalid"))?;
        if principal != &expected.controller_principal || *generation != owner.identity.generation {
            return Err(error("accepted_owner_changed"));
        }
        let task = store.external_task(permit.scope_ref, permit.work)?;
        if task.specialist_run_id != permit.target {
            return Err(error("accepted_target_changed"));
        }
        let accepted = store
            .external_task_identity(permit.scope_ref, permit.work)?
            .ok_or_else(|| error("accepted_identity_missing"))?;
        let (intent, seconds) =
            store.external_task_dispatch_facts(permit.scope_ref, permit.work)?;
        if intent != accepted.execution_intent {
            return Err(error("accepted_intent_changed"));
        }
        let origin: i64 = c
            .query_row(
                "SELECT created_at_ms FROM tasks WHERE task_id=?1",
                [permit.work.to_string()],
                |r| r.get(0),
            )
            .map_err(sql)?;
        let recorded = store
            .accepted_assignment_body_digest(
                workspace,
                permit.scope_ref,
                &receipt.namespace.idempotency_key,
            )?
            .ok_or_else(|| error("accepted_body_unbound"))?;
        (
            accepted,
            receipt.namespace.idempotency_key,
            seconds,
            origin,
            recorded,
        )
    } else {
        let root =
            crate::controller::load_reconciled_controller_binding(state_root, permit.scope_ref)?;
        let expected = crate::operation::Namespace::delegation(
            &root.identity,
            workspace,
            Some(("orchestrated_session", &permit.scope_ref.to_string())),
            operation,
            &receipt.namespace.idempotency_key,
        )
        .map_err(|_| error("accepted_owner_namespace_invalid"))?;
        if principal != &expected.controller_principal || *generation != root.identity.generation {
            return Err(error("accepted_owner_changed"));
        }
        let task = crate::orchestration::successor_task(c, permit.scope_ref, permit.work)?
            .ok_or_else(|| error("accepted_identity_missing"))?;
        if task.member_ref != permit.target.to_string() {
            return Err(error("accepted_target_changed"));
        }
        let accepted =
            crate::successor_capture::retained_accepted_task(&task.accepted_request_json)?;
        let marker: serde_json::Value = serde_json::from_str(&task.request_json)
            .map_err(|_| error("accepted_marker_invalid"))?;
        if marker["execution_intent"].as_str() != Some(accepted.execution_intent.as_str()) {
            return Err(error("accepted_intent_changed"));
        }
        let seconds = marker["deadline_seconds"]
            .as_u64()
            .ok_or_else(|| error("accepted_deadline_missing"))?;
        let (origin,digest):(i64,String)=c.query_row("SELECT deadline_origin_ms,accepted_request_sha256 FROM brokered_task_acceptances WHERE task_id=?1",[permit.work.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sql)?;
        if crate::jcs::sha256_hex(task.accepted_request_json.as_bytes()) != digest {
            return Err(error("accepted_identity_changed"));
        }
        (
            accepted,
            format!("successor-task:{}", permit.work),
            seconds,
            origin,
            format!("sha256:{}", task.source_request_sha256),
        )
    };
    let (task_body, contexts, target) = crate::engagement::accepted_task_content(c, permit.work)?
        .ok_or_else(|| error("accepted_bytes_missing"))?;
    let request: crate::task_request::SpecialistTaskRequest =
        serde_json::from_slice(&task_body).map_err(|_| error("accepted_body_invalid"))?;
    if !crate::successor_capture::verify_accepted_assignment(
        permit.target,
        &request,
        &accepted,
        deadline_seconds,
        &recorded,
    )? {
        return Err(error("accepted_body_changed"));
    }
    let contents = crate::successor_capture::AcceptedTaskContents {
        task_body,
        contexts,
        target,
    };
    contents.prompt(&accepted, &accepted.execution_intent, deadline_seconds)?;
    let deadline_ms = origin
        .checked_add(
            i64::try_from(deadline_seconds)
                .ok()
                .and_then(|n| n.checked_mul(1000))
                .ok_or_else(|| error("accepted_deadline_invalid"))?,
        )
        .ok_or_else(|| error("accepted_deadline_invalid"))?;
    Ok(RetainedTask {
        accepted,
        contents,
        key,
        deadline_seconds,
        deadline_ms,
    })
}

pub(crate) fn authorize_external_work(
    state_root: &Path,
    run: Uuid,
    scope: Uuid,
    operation: &str,
    permit: &AcceptedWork,
) -> Result<(), MachineError> {
    if permit.target != run
        || permit.scope_ref != scope
        || permit.scope_kind != "external_engagement"
        || !matches!(
            operation,
            "engagement.assign"
                | "engagement.acquire_write"
                | "engagement.release_write"
                | "run.verify_caller_policy"
        )
    {
        return Err(error("accepted_work_scope_invalid"));
    }
    let store = crate::engagement::EngagementStore::open_read_only(
        &crate::engagement::EngagementStore::workspace_database_path(state_root),
    )?;
    let current:(String,String,String,String,String,String,i64)=store.connection().query_row("SELECT kind,work_ref,target_run_id,scope_kind,scope_ref,phase,lease_until_ms FROM scheduler_claims WHERE claim_id=?1",[permit.claim.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).map_err(sql)?;
    if (&current.0, &current.1, &current.2, &current.3, &current.4)
        != (
            &permit.kind,
            &permit.work.to_string(),
            &run.to_string(),
            &permit.scope_kind,
            &scope.to_string(),
        )
        || !matches!(current.5.as_str(), "claimed" | "dispatching" | "accepted")
    {
        return Err(error("accepted_claim_changed"));
    }
    let manifest = crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, state_root)
        .load_manifest(run)?;
    let binding = manifest
        .aggregate_binding
        .as_ref()
        .ok_or_else(|| error("accepted_binding_missing"))?;
    let (workspace, owner) = store.accepted_external_owner(scope)?;
    let controller = crate::controller::load_reconciled_controller_binding(state_root, run)?;
    if manifest.schema_version != 4
        || manifest.workspace_id != workspace
        || manifest.controller != owner
        || controller != owner
        || binding.aggregate_id != scope
        || binding.aggregate_kind != crate::domain::AggregateKind::ExternalSpecialistEngagement
        || binding.member_kind != crate::run::AggregateMemberKind::Specialist
    {
        return Err(error("accepted_owner_changed"));
    }
    store.validate_external_member_binding(scope, run, binding)?;
    if !store.external_member_is_active(scope, run)? {
        return Err(error("accepted_member_inactive"));
    }
    let task = retained_task(&store, permit, &workspace, state_root)?;
    let state = store.external_task(scope, permit.work)?.state;
    if matches!(operation, "engagement.assign" | "engagement.acquire_write") {
        let active:bool=store.connection().query_row("SELECT EXISTS(SELECT 1 FROM engagements WHERE engagement_id=?1 AND state='active' AND authority_kind='external_v1')",[scope.to_string()],|r|r.get(0)).map_err(sql)?;
        if !active {
            return Err(error("accepted_owner_not_active"));
        }
    }
    if (operation == "engagement.assign" && state != "dispatching")
        || (operation == "engagement.acquire_write"
            && !matches!(state.as_str(), "queued" | "accepted"))
    {
        return Err(error("accepted_task_not_dispatchable"));
    }
    let (configuration, _, _) = store.external_member_configuration(scope, run)?;
    let digest = configuration
        .admitted_configuration
        .as_ref()
        .ok_or_else(|| error("accepted_configuration_missing"))?
        .digest()?;
    if manifest.agent_configuration != configuration
        || task.accepted.agent_configuration_digest != digest
        || manifest
            .instructions
            .delegation()
            .is_none_or(|instructions| {
                instructions.role_snapshot_digest != task.accepted.role_snapshot_digest
            })
    {
        return Err(error("accepted_configuration_changed"));
    }
    if operation != "engagement.release_write"
        && (now_ms()? >= task.deadline_ms || (current.5 == "claimed" && now_ms()? >= current.6))
    {
        return Err(error("accepted_claim_expired"));
    }
    if operation == "engagement.assign" && current.5 != "dispatching" {
        return Err(error("accepted_dispatch_unreserved"));
    }
    if matches!(
        operation,
        "engagement.acquire_write" | "engagement.release_write"
    ) && task.accepted.execution_intent != "canonical_workspace_write"
    {
        return Err(error("accepted_writer_forbidden"));
    }
    Ok(())
}

pub(crate) fn claim(
    tx: &Transaction<'_>,
    owner: Owner,
    target: Uuid,
    now: i64,
) -> Result<Option<AcceptedWork>, MachineError> {
    install(tx)?;
    let busy: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM scheduler_claims WHERE target_run_id=?1)",
            [target.to_string()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if busy {
        return Ok(None);
    }
    let active = match owner {
        Owner::Managed(_) => {
            "SELECT EXISTS(SELECT 1 FROM brokered_tasks WHERE target_run_id=?1 AND state IN ('accepted','claimed','dispatching','running','interrupted_unknown'))"
        }
        Owner::External(_) => {
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE specialist_run_id=?1 AND state IN ('accepted','claimed','dispatching','running','interrupted_unknown'))"
        }
    };
    if tx
        .query_row(active, [target.to_string()], |r| r.get::<_, bool>(0))
        .map_err(sql)?
    {
        return Ok(None);
    }
    if matches!(owner,Owner::Managed(_)) && table(tx,"collaboration_exchanges")? && tx.query_row("SELECT EXISTS(SELECT 1 FROM collaboration_exchanges WHERE target_run_id=?1 AND execution_state IN ('claimed','dispatching','running','interrupted_unknown'))",[target.to_string()],|r|r.get::<_,bool>(0)).map_err(sql)? { return Ok(None); }
    let history:Option<(String,u8,u32)>=tx.query_row("SELECT source_ref,effective_priority,consecutive FROM scheduler_fairness WHERE target_run_id=?1",[target.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql)?;
    let policy = policy(tx, owner)?;
    let Some(next) = select(
        candidates(tx, owner, target)?,
        now,
        history.as_ref().map(|(s, p, n)| (s.as_str(), *p, *n)),
        policy,
    ) else {
        return Ok(None);
    };
    let (scope_kind, scope_ref) = owner.scope();
    let permit = AcceptedWork {
        schema: "dolgorae.accepted-work/v1".into(),
        claim: Uuid::now_v7(),
        work: next.id,
        target,
        scope_kind: scope_kind.into(),
        scope_ref,
        kind: next.kind,
    };
    tx.execute("INSERT INTO scheduler_claims(claim_id,kind,work_ref,target_run_id,scope_kind,scope_ref,lease_until_ms,phase) VALUES(?1,?2,?3,?4,?5,?6,?7,'claimed')",params![permit.claim.to_string(),permit.kind,permit.work.to_string(),target.to_string(),scope_kind,scope_ref.to_string(),now+policy.activation_ms]).map_err(sql)?;
    Ok(Some(permit))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wake {
    workspace: String,
    run: Uuid,
}

fn wake_path(home: &Path) -> Option<PathBuf> {
    crate::workspace::verify_secure_file(
        &home.join("rpc/gateway.json"),
        crate::darwin::DarwinSystem.current_uid(),
    )
    .ok()?;
    let record: crate::gateway_socket::GatewayRecord =
        serde_json::from_slice(&std::fs::read(home.join("rpc/gateway.json")).ok()?).ok()?;
    Some(wake_socket_path(home, record.server_instance_id))
}

fn wake_socket_path(home: &Path, instance: Uuid) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    let mut preimage = b"dolgorae-scheduler-wake-v1\0".to_vec();
    preimage.extend_from_slice(home.as_os_str().as_bytes());
    preimage.push(0);
    preimage.extend_from_slice(instance.as_bytes());
    let uid = crate::darwin::DarwinSystem.current_uid();
    PathBuf::from(format!(
        "/tmp/dolgorae-{uid}/w/{}.sock",
        crate::jcs::sha256_hex(&preimage)
    ))
}

fn bind_wake_socket(
    home: &Path,
    instance: Uuid,
) -> Result<(PathBuf, std::os::unix::net::UnixDatagram), MachineError> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = wake_socket_path(home, instance);
    let parent = path.parent().ok_or_else(|| error("wake_parent_missing"))?;
    crate::worker::prepare_socket_root(crate::darwin::DarwinSystem.current_uid(), parent)
        .map_err(|_| error("wake_root_invalid"))?;
    // Only this fresh gateway instance can own this name. A collision is a
    // refusal, never permission to unlink an unverified endpoint.
    let socket =
        std::os::unix::net::UnixDatagram::bind(&path).map_err(|_| error("wake_bind_failed"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| error("wake_permissions_failed"))?;
    Ok((path, socket))
}

/// Best-effort post-commit hint. Failure leaves durable work untouched.
pub(crate) fn notify(state_root: &Path, run: Uuid) {
    let Some(home) = state_root.parent().and_then(Path::parent) else {
        return;
    };
    let Some(workspace) = state_root.file_name().and_then(|s| s.to_str()) else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(&Wake {
        workspace: workspace.into(),
        run,
    }) else {
        return;
    };
    if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
        let _ = socket.set_nonblocking(true);
        if let Some(path) = wake_path(home) {
            let parent = path.parent().expect("fixed wake path parent");
            let root = parent.parent().expect("fixed private socket root");
            let uid = crate::darwin::DarwinSystem.current_uid();
            // A sender only observes the root. Gateway startup owns creation;
            // a missing or replaced directory leaves the durable wake pending.
            if crate::workspace::verify_secure_directory(root, uid).is_ok()
                && crate::workspace::verify_secure_directory(parent, uid).is_ok()
            {
                let _ = socket.send_to(&bytes, path);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Actor {
    pub owner: Owner,
    pub run: Uuid,
}

pub(crate) fn actors(c: &Connection) -> Result<Vec<Actor>, MachineError> {
    let mut actors = Vec::new();
    for (present, query, managed) in [
        (
            table(c, "brokered_members")?,
            "SELECT m.session_id,m.run_id FROM brokered_members m JOIN orchestrated_sessions s USING(session_id) WHERE m.membership_state='active' AND s.status='active'",
            true,
        ),
        (
            table(c, "members")?,
            "SELECT m.engagement_id,m.specialist_run_id FROM members m JOIN engagements e USING(engagement_id) WHERE m.state='active' AND e.state='active' AND e.authority_kind='external_v1'",
            false,
        ),
    ] {
        if !present {
            continue;
        }
        let mut s = c.prepare(query).map_err(sql)?;
        for row in s
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(sql)?
        {
            let (scope, run) = row.map_err(sql)?;
            let scope = Uuid::parse_str(&scope).map_err(|_| error("invalid_actor_owner"))?;
            actors.push(Actor {
                owner: if managed {
                    Owner::Managed(scope)
                } else {
                    Owner::External(scope)
                },
                run: Uuid::parse_str(&run).map_err(|_| error("invalid_actor_identity"))?,
            });
        }
    }
    let mut enrolled = Vec::new();
    for actor in actors {
        let run = actor.run.to_string();
        let seen = table(c, "scheduler_actors")?
            && c.query_row(
                "SELECT EXISTS(SELECT 1 FROM scheduler_actors WHERE target_run_id=?1)",
                [&run],
                |r| r.get::<_, bool>(0),
            )
            .map_err(sql)?;
        let assigned = table(c, "assignment_queue_entries")?
            && c.query_row(
                "SELECT EXISTS(SELECT 1 FROM assignment_queue_entries WHERE target_run_id=?1)",
                [&run],
                |r| r.get::<_, bool>(0),
            )
            .map_err(sql)?;
        let consulted = table(c, "collaboration_mailbox")?
            && c.query_row(
                "SELECT EXISTS(SELECT 1 FROM collaboration_mailbox WHERE target_run_id=?1)",
                [&run],
                |r| r.get::<_, bool>(0),
            )
            .map_err(sql)?;
        if seen || assigned || consulted {
            enrolled.push(actor);
        }
    }
    Ok(enrolled)
}

pub(crate) fn residency(c: &Connection, actor: Actor) -> Result<String, MachineError> {
    let query = match actor.owner {
        Owner::Managed(_) => {
            "SELECT m.actor_residency FROM brokered_members m JOIN orchestrated_sessions s USING(session_id) WHERE m.session_id=?1 AND m.run_id=?2 AND m.membership_state='active' AND s.status='active'"
        }
        Owner::External(_) => {
            "SELECT m.actor_residency FROM members m JOIN engagements e USING(engagement_id) WHERE m.engagement_id=?1 AND m.specialist_run_id=?2 AND m.state='active' AND e.state='active'"
        }
    };
    c.query_row(
        query,
        params![actor.owner.scope().1.to_string(), actor.run.to_string()],
        |r| r.get(0),
    )
    .map_err(sql)
}

pub(crate) fn event(
    tx: &Transaction<'_>,
    owner: Owner,
    kind: &str,
    payload: &str,
    now: i64,
) -> Result<(), MachineError> {
    let digest = crate::jcs::sha256_hex(payload.as_bytes());
    match owner {
        Owner::Managed(scope) => crate::orchestration::append_event(tx, scope, kind, &digest, now),
        Owner::External(scope) => crate::engagement::append_event(tx, scope, kind, &digest),
    }
}

fn change_residency(
    tx: &Transaction<'_>,
    actor: Actor,
    from: &str,
    to: &str,
    now: i64,
) -> Result<(), MachineError> {
    let query = match actor.owner {
        Owner::Managed(_) => {
            "UPDATE brokered_members SET actor_residency=?3,updated_at_ms=?5 WHERE session_id=?1 AND run_id=?2 AND actor_residency=?4 AND membership_state='active'"
        }
        Owner::External(_) => {
            "UPDATE members SET actor_residency=?3,updated_at_ms=?5 WHERE engagement_id=?1 AND specialist_run_id=?2 AND actor_residency=?4 AND state='active'"
        }
    };
    if tx
        .execute(
            query,
            params![
                actor.owner.scope().1.to_string(),
                actor.run.to_string(),
                to,
                from,
                now
            ],
        )
        .map_err(sql)?
        != 1
    {
        return Err(error("actor_residency_changed"));
    }
    event(
        tx,
        actor.owner,
        "actor_residency_changed",
        &format!("{}\0{from}\0{to}", actor.run),
        now,
    )
}

pub(crate) fn observe_actor(
    tx: &Transaction<'_>,
    actor: Actor,
    revision: u64,
    idle: bool,
    now: i64,
) -> Result<i64, MachineError> {
    install(tx)?;
    let (kind, scope) = actor.owner.scope();
    tx.execute("INSERT INTO scheduler_actors(target_run_id,scope_kind,scope_ref,observed_revision,idle_since_ms) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(target_run_id) DO UPDATE SET idle_since_ms=CASE WHEN observed_revision<>excluded.observed_revision OR ?6 THEN excluded.idle_since_ms ELSE idle_since_ms END,observed_revision=excluded.observed_revision",params![actor.run.to_string(),kind,scope.to_string(),revision,now,!idle]).map_err(sql)?;
    tx.query_row(
        "SELECT idle_since_ms FROM scheduler_actors WHERE target_run_id=?1",
        [actor.run.to_string()],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// These guards consume existing durable work/delivery facts. TASK-083 will
/// supply actual blocking wait edges; absence of that not-yet-enabled plane
/// does not manufacture an acknowledgment for a pending result.
pub(crate) fn idle_mail_guards(c: &Connection, actor: Actor) -> Result<(), MachineError> {
    if !candidates(c, actor.owner, actor.run)?.is_empty() {
        return Err(error("actor_has_mail"));
    }
    let claims: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM scheduler_claims WHERE target_run_id=?1)",
            [actor.run.to_string()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if claims {
        return Err(error("actor_has_claim"));
    }
    let query = match actor.owner {
        Owner::Managed(_) => {
            "SELECT EXISTS(SELECT 1 FROM brokered_tasks WHERE target_run_id=?1 AND state IN ('accepted','claimed','dispatching','running','interrupted_unknown','completed_not_delivered'))"
        }
        Owner::External(_) => {
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE specialist_run_id=?1 AND state IN ('accepted','claimed','dispatching','running','interrupted_unknown','completed_not_delivered'))"
        }
    };
    if c.query_row(query, [actor.run.to_string()], |r| r.get::<_, bool>(0))
        .map_err(sql)?
    {
        return Err(error("actor_has_unsettled_task"));
    }
    if matches!(actor.owner, Owner::Managed(_)) && table(c, "collaboration_exchanges")? {
        let pending:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM collaboration_exchanges WHERE (target_run_id=?1 AND (delivery_state='pending' OR execution_state IN ('claimed','dispatching','running','interrupted_unknown'))) OR (source_run_id=?1 AND (delivery_state='pending' OR execution_state IN ('queued','claimed','dispatching','running','interrupted_unknown'))))",[actor.run.to_string()],|r|r.get(0)).map_err(sql)?;
        if pending {
            return Err(error("actor_has_outbound_or_pending_result"));
        }
    }
    Ok(())
}

pub(crate) fn begin_activation(
    tx: &Transaction<'_>,
    actor: Actor,
    now: i64,
) -> Result<Uuid, MachineError> {
    let residency = residency(tx, actor)?;
    if !matches!(residency.as_str(), "passivated" | "unstarted") {
        return Err(error("activation_not_passivated"));
    }
    let policy = policy(tx, actor.owner)?;
    let (attempts, operation): (u32, Option<String>) = tx
        .query_row(
            "SELECT attempts,operation_id FROM scheduler_actors WHERE target_run_id=?1",
            [actor.run.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql)?;
    if operation.is_some() || attempts >= policy.attempts {
        return Err(error("activation_attempt_bound"));
    }
    let operation = Uuid::now_v7();
    change_residency(tx, actor, &residency, "activating", now)?;
    tx.execute("UPDATE scheduler_actors SET operation_id=?2,operation_kind='activation',lease_until_ms=?3,attempts=attempts+1,safe_blocker=NULL WHERE target_run_id=?1",params![actor.run.to_string(),operation.to_string(),now+policy.activation_ms]).map_err(sql)?;
    event(
        tx,
        actor.owner,
        "actor_activation_started",
        &operation.to_string(),
        now,
    )?;
    Ok(operation)
}

pub(crate) fn finish_activation(
    tx: &Transaction<'_>,
    actor: Actor,
    operation: Uuid,
    resident: bool,
    now: i64,
) -> Result<(), MachineError> {
    let until:i64=tx.query_row("SELECT lease_until_ms FROM scheduler_actors WHERE target_run_id=?1 AND operation_id=?2 AND operation_kind='activation'",params![actor.run.to_string(),operation.to_string()],|r|r.get(0)).map_err(sql)?;
    let success = resident && now < until;
    if success {
        let observed: Option<String> = tx
            .query_row(
                "SELECT observed_identity_json FROM scheduler_operations WHERE operation_id=?1",
                [operation.to_string()],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if observed.is_none() {
            return Err(error("activation_ready_observation_missing"));
        }
    }
    operation_outcome(
        tx,
        actor,
        &operation.to_string(),
        if success { "resident" } else { "unavailable" },
        if success {
            None
        } else {
            Some("activation_outcome_unverified")
        },
        now,
    )?;
    change_residency(
        tx,
        actor,
        "activating",
        if success { "resident" } else { "unavailable" },
        now,
    )?;
    tx.execute("UPDATE scheduler_actors SET operation_id=NULL,operation_kind=NULL,lease_until_ms=NULL,safe_blocker=?2,attempts=CASE WHEN ?3 THEN 0 ELSE attempts END WHERE target_run_id=?1",params![actor.run.to_string(),if success{None}else{Some("activation_outcome_unverified")},success]).map_err(sql)?;
    event(
        tx,
        actor.owner,
        "actor_activation_finished",
        &format!("{operation}\0{success}"),
        now,
    )
}

/// Called on the Worker drain while it is idle. Every later Turn admission
/// reads this durable fence, so shutdown cannot race a second accepted Turn.
pub(crate) fn fence_passivation(
    state_root: &Path,
    run: Uuid,
    operation: Uuid,
) -> Result<(), MachineError> {
    let snapshot = crate::snapshot::RunSnapshot::load(state_root, run, 0)?;
    if snapshot.projection.lifecycle != crate::domain::RunLifecycle::Idle
        || snapshot.projection.active_turn_id.is_some()
        || !snapshot.projection.pending_requests.is_empty()
        || snapshot.projection.thread_id.is_none()
        || snapshot
            .writer
            .holder
            .as_ref()
            .is_some_and(|h| h.run_id == run)
    {
        return Err(error("actor_not_quiescent"));
    }
    let binding = snapshot
        .manifest
        .aggregate_binding
        .as_ref()
        .ok_or_else(|| error("actor_binding_missing"))?;
    if binding.member_kind != crate::run::AggregateMemberKind::Specialist {
        return Err(error("actor_not_specialist"));
    }
    let actor = Actor {
        run,
        owner: match binding.aggregate_kind {
            crate::domain::AggregateKind::OrchestratedSession => {
                Owner::Managed(binding.aggregate_id)
            }
            crate::domain::AggregateKind::ExternalSpecialistEngagement => {
                Owner::External(binding.aggregate_id)
            }
        },
    };
    let mut store = crate::engagement::EngagementStore::open(
        &crate::engagement::EngagementStore::workspace_database_path(state_root),
    )?;
    store.transact(|tx| {
        let (stored,kind,until):(Option<String>,Option<String>,Option<i64>)=tx.query_row("SELECT operation_id,operation_kind,lease_until_ms FROM scheduler_actors WHERE target_run_id=?1",[run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sql)?;
        if stored.as_deref()!=Some(operation.to_string().as_str()) || kind.as_deref()!=Some("passivation") || until.is_none_or(|u|u<=now_ms().unwrap_or(i64::MAX)) {return Err(error("passivation_owner_changed"));}
        idle_mail_guards(tx,actor)?;
        change_residency(tx,actor,"resident","passivating",now_ms()?)
    })
}

pub(crate) fn require_turn_admission(state_root: &Path, run: Uuid) -> Result<(), MachineError> {
    let store = crate::engagement::EngagementStore::open_read_only(
        &crate::engagement::EngagementStore::workspace_database_path(state_root),
    )?;
    if !table(store.connection(), "scheduler_actors")? {
        return Ok(());
    }
    let fenced:bool=store.connection().query_row("SELECT EXISTS(SELECT 1 FROM scheduler_actors WHERE target_run_id=?1 AND operation_kind='passivation')",[run.to_string()],|r|r.get(0)).map_err(sql)?;
    if fenced {
        Err(error("actor_passivating"))
    } else {
        Ok(())
    }
}

pub(crate) fn prepare_dispatch(
    tx: &Transaction<'_>,
    permit: &AcceptedWork,
    now: i64,
) -> Result<(), MachineError> {
    let owner = if permit.scope_kind == "orchestrated_session" {
        Owner::Managed(permit.scope_ref)
    } else {
        Owner::External(permit.scope_ref)
    };
    let selected = candidates(tx, owner, permit.target)?
        .into_iter()
        .find(|c| c.id == permit.work && c.kind == permit.kind)
        .ok_or_else(|| error("claimed_work_changed"))?;
    if selected.owner != owner || selected.target != permit.target {
        return Err(error("claimed_owner_changed"));
    }
    let changed=tx.execute("UPDATE scheduler_claims SET phase='dispatching' WHERE claim_id=?1 AND work_ref=?2 AND target_run_id=?3 AND phase='claimed' AND lease_until_ms>?4",params![permit.claim.to_string(),permit.work.to_string(),permit.target.to_string(),now]).map_err(sql)?;
    if changed != 1 {
        return Err(error("dispatch_claim_expired"));
    }
    let query = match (permit.scope_kind.as_str(), permit.kind.as_str()) {
        ("orchestrated_session", "assignment") => {
            "UPDATE brokered_tasks SET state='accepted' WHERE task_id=?1 AND target_run_id=?2 AND state='queued'"
        }
        ("external_engagement", "assignment") => {
            "UPDATE tasks SET state='accepted' WHERE task_id=?1 AND specialist_run_id=?2 AND state='queued'"
        }
        ("orchestrated_session", "consultation") => {
            "UPDATE collaboration_exchanges SET execution_state='dispatching' WHERE exchange_id=?1 AND target_run_id=?2 AND execution_state='queued'"
        }
        _ => return Err(error("dispatch_scope_invalid")),
    };
    if tx
        .execute(
            query,
            params![permit.work.to_string(), permit.target.to_string()],
        )
        .map_err(sql)?
        != 1
    {
        return Err(error("dispatch_work_changed"));
    }
    let owner = if permit.scope_kind == "orchestrated_session" {
        Owner::Managed(permit.scope_ref)
    } else {
        Owner::External(permit.scope_ref)
    };
    let previous:Option<(String,u8,u32)>=tx.query_row("SELECT source_ref,effective_priority,consecutive FROM scheduler_fairness WHERE target_run_id=?1",[permit.target.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql)?;
    let count = previous
        .filter(|(source, _, _)| source == &selected.source)
        .map_or(1, |(_, _, n)| n.saturating_add(1));
    tx.execute("INSERT INTO scheduler_fairness(target_run_id,source_ref,effective_priority,consecutive) VALUES(?1,?2,?3,?4) ON CONFLICT(target_run_id) DO UPDATE SET source_ref=excluded.source_ref,effective_priority=excluded.effective_priority,consecutive=excluded.consecutive",params![permit.target.to_string(),selected.source,selected.priority,count]).map_err(sql)?;
    event(
        tx,
        owner,
        "scheduler_dispatch_reserved",
        &permit.claim.to_string(),
        now,
    )
}

pub(crate) fn finish_dispatch(
    tx: &Transaction<'_>,
    permit: &AcceptedWork,
    outcome: &crate::delegation_service::MemberDispatchOutcome,
    now: i64,
) -> Result<(), MachineError> {
    use crate::delegation_service::MemberDispatchOutcome;
    let (phase, turn, blocker) = match outcome {
        MemberDispatchOutcome::Accepted { turn_ref } => ("accepted", Some(turn_ref.as_str()), None),
        MemberDispatchOutcome::Refused { .. } => ("refused", None, Some("dispatch_refused")),
        MemberDispatchOutcome::Uncertain { .. } => {
            ("unknown", None, Some("dispatch_outcome_unknown"))
        }
    };
    tx.execute(
        "UPDATE scheduler_claims SET phase=?2,turn_id=?3,safe_blocker=?4 WHERE claim_id=?1",
        params![permit.claim.to_string(), phase, turn, blocker],
    )
    .map_err(sql)?;
    if let MemberDispatchOutcome::Accepted { turn_ref } = outcome {
        if permit.kind == "consultation" {
            // Result/outbox completion belongs to TASK-083. Keep the accepted
            // execution and its Turn even if that later hook is unavailable.
            tx.execute("UPDATE collaboration_exchanges SET execution_state='running' WHERE exchange_id=?1 AND execution_state='dispatching'",[permit.work.to_string()]).map_err(sql)?;
        } else {
            let owner = if permit.scope_kind == "orchestrated_session" {
                Owner::Managed(permit.scope_ref)
            } else {
                Owner::External(permit.scope_ref)
            };
            crate::assignment_queue::remove(tx, owner, permit.work)?;
        }
        let _ = turn_ref;
    }
    if phase == "refused" {
        // A proved nonacceptance keeps the original queue/receipt. No semantic
        // retry occurs during this scheduling visit.
        let query = match (permit.scope_kind.as_str(), permit.kind.as_str()) {
            (_, "consultation") => {
                "UPDATE collaboration_exchanges SET execution_state='queued' WHERE exchange_id=?1 AND execution_state='dispatching'"
            }
            ("orchestrated_session", _) => {
                "UPDATE brokered_tasks SET state='queued' WHERE task_id=?1 AND state IN ('accepted','dispatching') AND target_turn_id IS NULL"
            }
            _ => {
                "UPDATE tasks SET state='queued' WHERE task_id=?1 AND state IN ('accepted','dispatching') AND turn_id IS NULL"
            }
        };
        tx.execute(query, [permit.work.to_string()]).map_err(sql)?;
        tx.execute(
            "DELETE FROM scheduler_claims WHERE claim_id=?1",
            [permit.claim.to_string()],
        )
        .map_err(sql)?;
    }
    let owner = if permit.scope_kind == "orchestrated_session" {
        Owner::Managed(permit.scope_ref)
    } else {
        Owner::External(permit.scope_ref)
    };
    event(
        tx,
        owner,
        "scheduler_dispatch_observed",
        &format!("{}\0{phase}", permit.claim),
        now,
    )
}

pub(crate) fn expire_queued(
    tx: &Transaction<'_>,
    actor: Actor,
    now: i64,
) -> Result<(), MachineError> {
    for candidate in candidates(tx, actor.owner, actor.run)?
        .into_iter()
        .filter(|c| c.deadline_ms <= now)
    {
        match (actor.owner, candidate.kind.as_str()) {
            (Owner::Managed(scope), "assignment") => {
                crate::orchestration::terminalize_successor_task(
                    tx,
                    scope,
                    candidate.id,
                    "OPERATION_TIMEOUT",
                    now,
                )?;
                crate::caller_scope::proved_nondispatch(tx, candidate.id)?;
                crate::assignment_queue::remove(tx, actor.owner, candidate.id)?;
            }
            (Owner::External(scope), "assignment") => {
                crate::engagement::terminalize_external_task_in(
                    tx,
                    crate::engagement::TaskTerminal {
                        engagement_id: scope,
                        task_id: candidate.id,
                        state: "expired",
                        safe_error_code: Some("OPERATION_TIMEOUT"),
                        result_sha256: None,
                        artifact_id: None,
                        diagnostic_json: None,
                        now_ms: now,
                    },
                )?;
                crate::caller_scope::proved_nondispatch(tx, candidate.id)?;
                crate::assignment_queue::remove(tx, actor.owner, candidate.id)?;
            }
            (Owner::Managed(_), "consultation") => {
                tx.execute("UPDATE collaboration_exchanges SET execution_state='expired' WHERE exchange_id=?1 AND execution_state='queued'",[candidate.id.to_string()]).map_err(sql)?;
            }
            _ => return Err(error("queued_scope_invalid")),
        }
        event(
            tx,
            actor.owner,
            "scheduler_queued_work_expired",
            &candidate.id.to_string(),
            now,
        )?;
    }
    Ok(())
}

/// A lease can be returned only while the durable pre-effect work and verified
/// Run ledger both exclude acceptance. Running/ambiguous claims stay fenced.
pub(crate) fn reconcile_claim(
    tx: &Transaction<'_>,
    actor: Actor,
    snapshot: &crate::snapshot::RunSnapshot,
    now: i64,
    state_root: &Path,
) -> Result<(), MachineError> {
    let row:Option<(String,String,String,i64,Option<String>,String)>=tx.query_row("SELECT claim_id,kind,work_ref,lease_until_ms,turn_id,phase FROM scheduler_claims WHERE target_run_id=?1",[actor.run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(sql)?;
    let Some((claim, kind, work, until, turn, phase)) = row else {
        return Ok(());
    };
    let work_id = Uuid::parse_str(&work).map_err(|_| error("claim_work_invalid"))?;
    let query = match (actor.owner, kind.as_str()) {
        (_, "consultation") => {
            "SELECT execution_state FROM collaboration_exchanges WHERE exchange_id=?1"
        }
        (Owner::Managed(_), _) => "SELECT state FROM brokered_tasks WHERE task_id=?1",
        (Owner::External(_), _) => "SELECT state FROM tasks WHERE task_id=?1",
    };
    let state: String = tx.query_row(query, [&work], |r| r.get(0)).map_err(sql)?;
    if matches!(
        state.as_str(),
        "completed_not_delivered" | "delivered" | "failed" | "cancelled" | "expired"
    ) && snapshot.projection.active_turn_id.is_none()
        && snapshot
            .writer
            .holder
            .as_ref()
            .is_none_or(|h| h.run_id != actor.run)
    {
        tx.execute("DELETE FROM scheduler_claims WHERE claim_id=?1", [&claim])
            .map_err(sql)?;
        return event(
            tx,
            actor.owner,
            "scheduler_terminal_claim_retired",
            &claim,
            now,
        );
    }
    if phase != "claimed"
        || until > now
        || turn.is_some()
        || !matches!(state.as_str(), "queued" | "accepted")
        || snapshot.projection.lifecycle != crate::domain::RunLifecycle::Idle
        || snapshot.projection.active_turn_id.is_some()
        || snapshot
            .writer
            .holder
            .as_ref()
            .is_some_and(|h| h.run_id == actor.run)
        || !snapshot.projection.pending_requests.is_empty()
    {
        return Ok(());
    }
    let ledger = snapshot.observed_ledger(state_root)?;
    let key=match actor.owner {
        Owner::Managed(_)=>format!("successor-task:{work}"),
        Owner::External(_)=>tx.query_row("SELECT idempotency_key FROM delegation_operations WHERE json_extract(accepted_subjects_json,'$.task_ref')=?1 AND state='accepted'",[&work],|r|r.get::<_,String>(0)).map_err(sql)?,
    };
    for record in ledger.records() {
        let value: serde_json::Value = record
            .payload_value()
            .map_err(|_| error("claim_ledger_invalid"))?;
        if value["idempotency_key"].as_str() == Some(&key)
            || value["mutation_envelope"]["idempotency_key"].as_str() == Some(&key)
        {
            return Ok(());
        }
    }
    let query = match actor.owner {
        Owner::Managed(_) => {
            "UPDATE brokered_tasks SET state='queued' WHERE task_id=?1 AND state='accepted' AND target_turn_id IS NULL"
        }
        Owner::External(_) => {
            "UPDATE tasks SET state='queued' WHERE task_id=?1 AND state='accepted' AND turn_id IS NULL"
        }
    };
    tx.execute(query, [&work]).map_err(sql)?;
    tx.execute("DELETE FROM scheduler_claims WHERE claim_id=?1", [&claim])
        .map_err(sql)?;
    event(
        tx,
        actor.owner,
        "scheduler_unaccepted_claim_returned",
        &format!("{claim}\0{work_id}"),
        now,
    )
}

pub(crate) struct ControlPlaneRuntime {
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    socket: PathBuf,
    identity: (u64, u64),
}

impl ControlPlaneRuntime {
    pub(crate) fn start(
        home: &crate::paths::DolgoraeHome,
        instance: Uuid,
    ) -> Result<Self, MachineError> {
        use std::os::unix::fs::MetadataExt as _;
        let (socket, listener) = bind_wake_socket(home.root(), instance)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| error("wake_bind_failed"))?;
        let listener =
            tokio::net::UnixDatagram::from_std(listener).map_err(|_| error("wake_bind_failed"))?;
        let meta =
            std::fs::symlink_metadata(&socket).map_err(|_| error("wake_identity_unavailable"))?;
        let identity = (meta.dev(), meta.ino());
        let home = home.clone();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let mut dirty = std::collections::BTreeSet::new();
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(30));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut bytes = [0_u8; 2049];
            loop {
                let reconcile = tokio::select! {
                    _=stopped.changed()=>break,
                    _=timer.tick()=>true,
                    message=listener.recv(&mut bytes)=>{
                        if let Ok(length)=message && length<=2048 && let Ok(wake)=serde_json::from_slice::<Wake>(&bytes[..length]) && valid_workspace_id(&wake.workspace) {
                            dirty.insert((wake.workspace,wake.run));
                        }
                        false
                    }
                };
                // Drain already queued hints before visiting the database. A
                // fixed bound prevents wake floods starving durable scans.
                for _ in 0..256 {
                    match listener.try_recv(&mut bytes) {
                        Ok(length) if length <= 2048 => {
                            if let Ok(wake) = serde_json::from_slice::<Wake>(&bytes[..length])
                                && valid_workspace_id(&wake.workspace)
                            {
                                dirty.insert((wake.workspace, wake.run));
                            }
                        }
                        _ => break,
                    }
                }
                let batch = std::mem::take(&mut dirty);
                let home = home.clone();
                // One bounded gateway job owns the current pass. There is no
                // actor task, timer, model loop or authoritative memory registry.
                let _ =
                    tokio::task::spawn_blocking(move || reconcile_home(&home, batch, reconcile))
                        .await;
            }
        });
        Ok(Self {
            stop,
            task,
            socket,
            identity,
        })
    }

    pub(crate) async fn shutdown(self) {
        use std::os::unix::fs::MetadataExt as _;
        let _ = self.stop.send(true);
        // Claims/operations commit before every external effect. A foreground
        // drain timeout leaves their durable uncertainty for restart recovery.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(35), self.task).await;
        if std::fs::symlink_metadata(&self.socket)
            .is_ok_and(|m| (m.dev(), m.ino()) == self.identity)
        {
            let _ = std::fs::remove_file(self.socket);
        }
    }
}

fn valid_workspace_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn registered_workspace(
    home: &crate::paths::DolgoraeHome,
    id: &str,
) -> Result<crate::workspace::WorkspaceView, MachineError> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if !valid_workspace_id(id) {
        return Err(error("workspace_identity_invalid"));
    }
    let root = home.workspace_root(id);
    crate::workspace::verify_secure_directory(&root, crate::darwin::DarwinSystem.current_uid())?;
    let path = root.join("workspace.json");
    crate::workspace::verify_secure_file(&path, crate::darwin::DarwinSystem.current_uid())?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| error("workspace_record_unavailable"))?;
    let mut bytes = Vec::new();
    file.take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|_| error("workspace_record_unreadable"))?;
    if bytes.len() > 1_048_576 {
        return Err(error("workspace_record_oversized"));
    }
    let record: crate::workspace::WorkspaceRecord =
        serde_json::from_slice(&bytes).map_err(|_| error("workspace_record_invalid"))?;
    if record.workspace_id != id {
        return Err(error("workspace_identity_changed"));
    }
    let path = record.canonical_path.to_path_buf()?;
    let view = crate::workspace::WorkspaceService::new(
        crate::workspace::SystemWorkspacePlatform,
        crate::workspace::SystemGitRunner,
        home.root().to_owned(),
    )
    .discover(Some(&path))?;
    if view.workspace_id != id {
        return Err(error("workspace_identity_changed"));
    }
    Ok(view)
}

fn reconcile_home(
    home: &crate::paths::DolgoraeHome,
    mut dirty: std::collections::BTreeSet<(String, Uuid)>,
    full: bool,
) {
    if full && let Ok(entries) = std::fs::read_dir(home.root().join("workspaces")) {
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !valid_workspace_id(&id) || registered_workspace(home, &id).is_err() {
                continue;
            }
            let root = home.workspace_root(&id);
            let database = crate::engagement::EngagementStore::workspace_database_path(&root);
            if !database.exists() {
                continue;
            }
            let Ok(observer) = crate::engagement::EngagementStore::open_read_only(&database) else {
                continue;
            };
            if actors(observer.connection()).map_or(true, |actors| actors.is_empty()) {
                continue;
            }
            drop(observer);
            let Ok(mut store) = crate::engagement::EngagementStore::open(&database) else {
                continue;
            };
            let initialized = store.transact(|tx| {
                install(tx)?;
                actors(tx)
            });
            if let Ok(actors) = initialized {
                for actor in actors {
                    dirty.insert((id.clone(), actor.run));
                }
            }
        }
    }
    for (workspace, run) in dirty {
        let Ok(view) = registered_workspace(home, &workspace) else {
            continue;
        };
        let root = home.workspace_root(&workspace);
        let Ok(store) = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&root),
        ) else {
            continue;
        };
        let Ok(actors) = actors(store.connection()) else {
            continue;
        };
        let Some(actor) = actors.into_iter().find(|a| a.run == run) else {
            continue;
        };
        drop(store);
        // The semantic boundary owns actual Run/process effects. Failures leave
        // durable mail, claim or operation evidence for the next global scan.
        if let Err(failure) = crate::semantic::schedule_actor(&view, &root, actor)
            && let Ok(mut store) = crate::engagement::EngagementStore::open(
                &crate::engagement::EngagementStore::workspace_database_path(&root),
            )
        {
            // Persist only a closed safe code. Detailed errors may contain
            // retained task or transport data and are never actor projections.
            let code = match failure.code.as_str() {
                "RUN_BUSY" => "run_busy",
                "STARTUP_BUSY" => "lifecycle_busy",
                _ => "actor_reconciliation_required",
            };
            let _ = store.transact(|tx| {
                let previous: Option<String> = tx
                    .query_row(
                        "SELECT safe_blocker FROM scheduler_actors WHERE target_run_id=?1",
                        [actor.run.to_string()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sql)?
                    .flatten();
                if previous.as_deref() != Some(code) {
                    tx.execute(
                        "UPDATE scheduler_actors SET safe_blocker=?2 WHERE target_run_id=?1",
                        params![actor.run.to_string(), code],
                    )
                    .map_err(sql)?;
                    event(
                        tx,
                        actor.owner,
                        "actor_scheduler_blocked",
                        &format!("{}\0{code}", actor.run),
                        now_ms()?,
                    )?;
                }
                Ok(())
            });
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Policy {
    pub starvation_ms: i64,
    pub consecutive: u32,
    pub idle_ms: i64,
    pub activation_ms: i64,
    pub attempts: u32,
}

pub(crate) fn policy(tx: &Transaction<'_>, owner: Owner) -> Result<Policy, MachineError> {
    let value = match owner {
        Owner::Managed(scope) => {
            crate::collaboration::install(tx)?;
            crate::collaboration::policy(tx, scope)?
        }
        Owner::External(scope) => {
            let value = serde_json::json!({"starvation_limit_seconds":300,"max_consecutive_from_same_source":2,"idle_passivation_seconds":600,"activation_timeout_seconds":30,"max_activation_attempts":3,"reconciliation_interval_seconds":30});
            let text = value.to_string();
            let hash = crate::jcs::sha256_hex(text.as_bytes());
            let stored:Option<(String,String)>=tx.query_row("SELECT policy_json,policy_sha256 FROM scheduler_external_policies WHERE scope_ref=?1",[scope.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
            if let Some((stored, digest)) = stored {
                if stored != text || digest != hash {
                    return Err(error("scheduler_policy_changed"));
                }
            } else {
                tx.execute("INSERT INTO scheduler_external_policies(scope_ref,policy_json,policy_sha256) VALUES(?1,?2,?3)",params![scope.to_string(),text,hash]).map_err(sql)?;
            }
            value
        }
    };
    let number = |key: &str| {
        value[key]
            .as_i64()
            .ok_or_else(|| error("scheduler_policy_invalid"))
    };
    if number("reconciliation_interval_seconds")? != 30 {
        return Err(error("scheduler_policy_unsupported"));
    }
    Ok(Policy {
        starvation_ms: number("starvation_limit_seconds")? * 1000,
        consecutive: u32::try_from(number("max_consecutive_from_same_source")?)
            .map_err(|_| error("scheduler_policy_invalid"))?,
        idle_ms: number("idle_passivation_seconds")? * 1000,
        activation_ms: number("activation_timeout_seconds")? * 1000,
        attempts: u32::try_from(number("max_activation_attempts")?)
            .map_err(|_| error("scheduler_policy_invalid"))?,
    })
}

pub(crate) fn require_live_dispatch_owner(
    c: &Connection,
    owner: Owner,
    run: Uuid,
) -> Result<(), MachineError> {
    if residency(c, Actor { owner, run })? != "resident" {
        return Err(error("queued_dispatch_member_not_resident"));
    }
    Ok(())
}

pub(crate) fn require_dispatch_claim(
    c: &Connection,
    task: Uuid,
    run: Uuid,
) -> Result<(), MachineError> {
    if !table(c, "assignment_queue_entries")? {
        return Ok(());
    }
    let queued: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM assignment_queue_entries WHERE task_id=?1)",
            [task.to_string()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if !queued {
        return Ok(());
    }
    if !table(c, "scheduler_claims")? {
        return Err(error("queued_dispatch_unclaimed"));
    }
    let (kind,scope,deadline):(String,String,i64)=c.query_row("SELECT scope_kind,scope_ref,deadline_ms FROM assignment_queue_entries WHERE task_id=?1 AND target_run_id=?2",params![task.to_string(),run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sql)?;
    let scope = Uuid::parse_str(&scope).map_err(|_| error("queued_dispatch_owner_invalid"))?;
    let owner = match kind.as_str() {
        "orchestrated_session" => Owner::Managed(scope),
        "external_engagement" => Owner::External(scope),
        _ => return Err(error("queued_dispatch_owner_invalid")),
    };
    require_live_dispatch_owner(c, owner, run)?;
    if now_ms()? >= deadline {
        return Err(error("queued_dispatch_deadline_expired"));
    }
    let claimed:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM scheduler_claims WHERE kind='assignment' AND work_ref=?1 AND target_run_id=?2 AND scope_kind=?3 AND scope_ref=?4 AND phase='dispatching')",params![task.to_string(),run.to_string(),kind,scope.to_string()],|r|r.get(0)).map_err(sql)?;
    if claimed {
        Ok(())
    } else {
        Err(error("queued_dispatch_claim_expired"))
    }
}

pub(crate) fn complete_passivation(
    tx: &Transaction<'_>,
    actor: Actor,
    now: i64,
) -> Result<(), MachineError> {
    let operation:Option<String>=tx.query_row("SELECT operation_id FROM scheduler_actors WHERE target_run_id=?1 AND operation_kind='passivation'",[actor.run.to_string()],|r|r.get(0)).map_err(sql)?;
    let operation = operation.ok_or_else(|| error("passivation_owner_missing"))?;
    operation_outcome(tx, actor, &operation, "passivated", None, now)?;
    change_residency(tx, actor, "passivating", "passivated", now)?;
    tx.execute("UPDATE scheduler_actors SET operation_id=NULL,operation_kind=NULL,lease_until_ms=NULL,safe_blocker=NULL WHERE target_run_id=?1 AND operation_id=?2",params![actor.run.to_string(),operation]).map_err(sql)?;
    event(
        tx,
        actor.owner,
        "actor_passivation_finished",
        &operation,
        now,
    )
}

pub(crate) fn activation_remaining(
    state_root: &Path,
    run: Uuid,
) -> Result<Option<std::time::Duration>, MachineError> {
    let path = crate::engagement::EngagementStore::workspace_database_path(state_root);
    if !path.exists() {
        return Ok(None);
    }
    let store = crate::engagement::EngagementStore::open_read_only(&path)?;
    if !table(store.connection(), "scheduler_actors")? {
        return Ok(None);
    }
    let until:Option<i64>=store.connection().query_row("SELECT lease_until_ms FROM scheduler_actors WHERE target_run_id=?1 AND operation_kind='activation'",[run.to_string()],|r|r.get(0)).optional().map_err(sql)?.flatten();
    until
        .map(|until| {
            until
                .checked_sub(now_ms()?)
                .filter(|ms| *ms > 0)
                .map(|ms| std::time::Duration::from_millis(ms as u64))
                .ok_or_else(|| error("activation_lease_expired"))
        })
        .transpose()
}

pub(crate) fn resume_or_claim(
    tx: &Transaction<'_>,
    owner: Owner,
    target: Uuid,
    now: i64,
) -> Result<Option<AcceptedWork>, MachineError> {
    let (scope_kind, scope_ref) = owner.scope();
    let row:Option<(String,String,String)>=tx.query_row("SELECT claim_id,kind,work_ref FROM scheduler_claims WHERE target_run_id=?1 AND scope_kind=?2 AND scope_ref=?3 AND phase='claimed' AND lease_until_ms>?4",params![target.to_string(),scope_kind,scope_ref.to_string(),now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql)?;
    if let Some((claim, kind, work)) = row {
        return Ok(Some(AcceptedWork {
            schema: "dolgorae.accepted-work/v1".into(),
            claim: Uuid::parse_str(&claim).map_err(|_| error("claim_identity_invalid"))?,
            work: Uuid::parse_str(&work).map_err(|_| error("claim_work_invalid"))?,
            target,
            scope_kind: scope_kind.into(),
            scope_ref,
            kind,
        }));
    }
    claim(tx, owner, target, now)
}

pub(crate) fn capture_operation(
    tx: &Transaction<'_>,
    actor: Actor,
    operation: Uuid,
    text: &str,
    now: i64,
) -> Result<(), MachineError> {
    if tx.execute("UPDATE scheduler_actors SET operation_snapshot_json=?3 WHERE target_run_id=?1 AND operation_id=?2",params![actor.run.to_string(),operation.to_string(),text]).map_err(sql)?!=1{return Err(error("actor_operation_changed"));}
    tx.execute("INSERT INTO scheduler_operations(operation_id,target_run_id,scope_kind,scope_ref,kind,lease_until_ms,attempt,identity_json,identity_sha256,state) SELECT operation_id,target_run_id,scope_kind,scope_ref,operation_kind,lease_until_ms,attempts,?3,?4,'prepared' FROM scheduler_actors WHERE target_run_id=?1 AND operation_id=?2",params![actor.run.to_string(),operation.to_string(),text,crate::jcs::sha256_hex(text.as_bytes())]).map_err(sql)?;
    event(
        tx,
        actor.owner,
        "actor_operation_captured",
        &format!("{operation}\0{}", crate::jcs::sha256_hex(text.as_bytes())),
        now,
    )
}

pub(crate) fn record_ready(
    tx: &Transaction<'_>,
    actor: Actor,
    operation: Uuid,
    text: &str,
    now: i64,
) -> Result<(), MachineError> {
    captured_operation(tx, actor, &operation.to_string())?;
    if tx.execute("UPDATE scheduler_operations SET observed_identity_json=?2 WHERE operation_id=?1 AND state='prepared' AND observed_identity_json IS NULL",params![operation.to_string(),text]).map_err(sql)?!=1{return Err(error("activation_ready_already_recorded"));}
    event(
        tx,
        actor.owner,
        "actor_ready_observed",
        &format!("{operation}\0{}", crate::jcs::sha256_hex(text.as_bytes())),
        now,
    )
}

pub(crate) fn captured_operation(
    c: &Connection,
    actor: Actor,
    operation: &str,
) -> Result<String, MachineError> {
    let (text,digest,current):(String,String,String)=c.query_row("SELECT o.identity_json,o.identity_sha256,a.operation_snapshot_json FROM scheduler_operations o JOIN scheduler_actors a USING(target_run_id) WHERE o.target_run_id=?1 AND o.operation_id=?2 AND a.operation_id=o.operation_id AND o.state='prepared'",params![actor.run.to_string(),operation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sql)?;
    verify_operation_capture(c, actor, operation, text, digest, current)
}

pub(crate) fn failed_activation_capture(
    c: &Connection,
    actor: Actor,
    operation: &str,
) -> Result<String, MachineError> {
    if residency(c, actor)? != "unavailable" {
        return Err(error("activation_cleanup_owner_changed"));
    }
    let (text,digest,current):(String,String,String)=c.query_row("SELECT o.identity_json,o.identity_sha256,a.operation_snapshot_json FROM scheduler_operations o JOIN scheduler_actors a USING(target_run_id) WHERE o.target_run_id=?1 AND o.operation_id=?2 AND o.scope_kind=?3 AND o.scope_ref=?4 AND o.kind='activation' AND o.state='unavailable' AND a.operation_id IS NULL",params![actor.run.to_string(),operation,actor.owner.scope().0,actor.owner.scope().1.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sql)?;
    verify_operation_capture(c, actor, operation, text, digest, current)
}

fn verify_operation_capture(
    c: &Connection,
    actor: Actor,
    operation: &str,
    text: String,
    digest: String,
    current: String,
) -> Result<String, MachineError> {
    if current != text || crate::jcs::sha256_hex(text.as_bytes()) != digest {
        return Err(error("actor_capture_changed"));
    }
    let payload = crate::jcs::sha256_hex(
        format!("{operation}\0{}", crate::jcs::sha256_hex(text.as_bytes())).as_bytes(),
    );
    let digest = if matches!(actor.owner, Owner::External(_)) {
        crate::jcs::sha256_hex(payload.as_bytes())
    } else {
        payload
    };
    let query = match actor.owner {
        Owner::Managed(_) => {
            "SELECT COUNT(*) FROM orchestration_events WHERE session_id=?1 AND kind='actor_operation_captured' AND payload_sha256=?2"
        }
        Owner::External(_) => {
            "SELECT COUNT(*) FROM events WHERE engagement_id=?1 AND kind='actor_operation_captured' AND payload_sha256=?2"
        }
    };
    let count: u64 = c
        .query_row(
            query,
            params![actor.owner.scope().1.to_string(), digest],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if count != 1 {
        return Err(error("actor_capture_event_missing"));
    }
    Ok(text)
}

pub(crate) fn settle_unfenced_passivation(
    tx: &Transaction<'_>,
    actor: Actor,
    now: i64,
) -> Result<bool, MachineError> {
    let row:Option<(String,i64)>=tx.query_row("SELECT operation_id,lease_until_ms FROM scheduler_actors WHERE target_run_id=?1 AND operation_kind='passivation'",[actor.run.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
    let Some((operation, until)) = row else {
        return Ok(false);
    };
    if residency(tx, actor)? != "resident" {
        return Err(error("actor_passivation_state_changed"));
    }
    if until <= now {
        operation_outcome(
            tx,
            actor,
            &operation,
            "not_fenced",
            Some("passivation_not_fenced"),
            now,
        )?;
        tx.execute("UPDATE scheduler_actors SET operation_id=NULL,operation_kind=NULL,lease_until_ms=NULL,safe_blocker='passivation_not_fenced' WHERE target_run_id=?1 AND operation_id=?2",params![actor.run.to_string(),operation]).map_err(sql)?;
    }
    Ok(true)
}

pub(crate) fn begin_idle_recovery(
    tx: &Transaction<'_>,
    actor: Actor,
    text: &str,
    now: i64,
) -> Result<Uuid, MachineError> {
    let policy = policy(tx, actor.owner)?;
    change_residency(tx, actor, "resident", "recovering", now)?;
    let operation = Uuid::now_v7();
    if tx.execute("UPDATE scheduler_actors SET operation_id=?2,operation_kind='recovery_detach',lease_until_ms=?3,safe_blocker='worker_generation_absent' WHERE target_run_id=?1 AND operation_id IS NULL",params![actor.run.to_string(),operation.to_string(),now+policy.activation_ms]).map_err(sql)?!=1{return Err(error("actor_operation_busy"));}
    capture_operation(tx, actor, operation, text, now)?;
    Ok(operation)
}

pub(crate) fn finish_idle_recovery(
    tx: &Transaction<'_>,
    actor: Actor,
    operation: &str,
    now: i64,
) -> Result<(), MachineError> {
    let matches:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM scheduler_actors WHERE target_run_id=?1 AND operation_id=?2 AND operation_kind='recovery_detach')",params![actor.run.to_string(),operation],|r|r.get(0)).map_err(sql)?;
    if !matches {
        return Err(error("actor_recovery_owner_changed"));
    }
    operation_outcome(tx, actor, operation, "passivated", None, now)?;
    change_residency(tx, actor, "recovering", "passivated", now)?;
    tx.execute("UPDATE scheduler_actors SET operation_id=NULL,operation_kind=NULL,lease_until_ms=NULL,safe_blocker='worker_generation_absent' WHERE target_run_id=?1",[actor.run.to_string()]).map_err(sql)?;
    event(
        tx,
        actor.owner,
        "actor_idle_crash_reconciled",
        operation,
        now,
    )
}

pub(crate) fn has_in_flight(c: &Connection, actor: Actor) -> Result<bool, MachineError> {
    let query = match actor.owner {
        Owner::Managed(_) => {
            "SELECT EXISTS(SELECT 1 FROM brokered_tasks WHERE target_run_id=?1 AND state IN ('accepted','claimed','dispatching','running','interrupted_unknown'))"
        }
        Owner::External(_) => {
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE specialist_run_id=?1 AND state IN ('accepted','claimed','dispatching','running','interrupted_unknown'))"
        }
    };
    if c.query_row(query, [actor.run.to_string()], |r| r.get::<_, bool>(0))
        .map_err(sql)?
    {
        return Ok(true);
    }
    if matches!(actor.owner,Owner::Managed(_)) && table(c,"collaboration_exchanges")? && c.query_row("SELECT EXISTS(SELECT 1 FROM collaboration_exchanges WHERE target_run_id=?1 AND execution_state IN ('claimed','dispatching','running','interrupted_unknown'))",[actor.run.to_string()],|r|r.get::<_,bool>(0)).map_err(sql)?{return Ok(true);}
    c.query_row("SELECT EXISTS(SELECT 1 FROM scheduler_claims WHERE target_run_id=?1 AND phase IN ('dispatching','accepted','unknown'))",[actor.run.to_string()],|r|r.get(0)).map_err(sql)
}

pub(crate) fn effect_remaining(
    state_root: &Path,
    run: Uuid,
) -> Result<Option<std::time::Duration>, MachineError> {
    let path = crate::engagement::EngagementStore::workspace_database_path(state_root);
    if !path.exists() {
        return Ok(None);
    }
    let store = crate::engagement::EngagementStore::open_read_only(&path)?;
    if !table(store.connection(), "scheduler_actors")? {
        return Ok(None);
    }
    let operation:Option<i64>=store.connection().query_row("SELECT lease_until_ms FROM scheduler_actors WHERE target_run_id=?1 AND operation_kind IN ('activation','passivation')",[run.to_string()],|r|r.get(0)).optional().map_err(sql)?.flatten();
    let claim: Option<(String, String, String, String, String, String, i64)> = store
        .connection()
        .query_row(
            "SELECT claim_id,kind,work_ref,scope_kind,scope_ref,phase,lease_until_ms
             FROM scheduler_claims WHERE target_run_id=?1 AND phase IN ('claimed','dispatching')",
            [run.to_string()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(sql)?;
    let claim = match claim {
        Some((_, _, _, _, _, phase, lease)) if phase == "claimed" => Some(lease),
        Some((claim, kind, work, scope_kind, scope, _, _)) => {
            let permit = AcceptedWork {
                schema: "dolgorae.accepted-work/v1".into(),
                claim: Uuid::parse_str(&claim).map_err(|_| error("claim_identity_invalid"))?,
                work: Uuid::parse_str(&work).map_err(|_| error("claim_work_invalid"))?,
                target: run,
                scope_kind,
                scope_ref: Uuid::parse_str(&scope).map_err(|_| error("claim_scope_invalid"))?,
                kind,
            };
            reserved_dispatch_deadline(&store, state_root, &permit)?
        }
        None => None,
    };
    let until = match (operation, claim) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    until
        .map(|until| {
            until
                .checked_sub(now_ms()?)
                .filter(|ms| *ms > 0)
                .map(|ms| std::time::Duration::from_millis(ms as u64))
                .ok_or_else(|| error("actor_effect_lease_expired"))
        })
        .transpose()
}

/// The reservation is no longer reclaimable. Its effects consume the original
/// acceptance budget, not the earlier claim-acquisition timer. Accepted or
/// terminal cleanup retains the ordinary operation timeout and release owner.
fn reserved_dispatch_deadline(
    store: &crate::engagement::EngagementStore,
    state_root: &Path,
    permit: &AcceptedWork,
) -> Result<Option<i64>, MachineError> {
    if permit.kind == "consultation" && permit.scope_kind == "orchestrated_session" {
        let work = crate::collaboration::execution_record(store.connection(), permit.work)?;
        if work.session != permit.scope_ref || work.target != permit.target {
            return Err(error("dispatch_consultation_changed"));
        }
        return Ok(Some(work.deadline_ms));
    }
    if permit.kind != "assignment" {
        return Err(error("dispatch_work_kind_invalid"));
    }
    let manifest = crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, state_root)
        .load_manifest(permit.target)?;
    let task = retained_task(store, permit, &manifest.workspace_id, state_root)?;
    let (state, turn) = match permit.scope_kind.as_str() {
        "external_engagement" => {
            let task = store.external_task(permit.scope_ref, permit.work)?;
            (task.state, task.turn_id)
        }
        "orchestrated_session" => {
            let task = crate::orchestration::successor_task(
                store.connection(),
                permit.scope_ref,
                permit.work,
            )?
            .ok_or_else(|| error("dispatch_task_missing"))?;
            (task.state, task.target_turn_id)
        }
        _ => return Err(error("dispatch_scope_invalid")),
    };
    if matches!(state.as_str(), "accepted" | "dispatching") && turn.is_none() {
        return Ok(Some(task.deadline_ms));
    }
    if matches!(
        state.as_str(),
        "running"
            | "completed_not_delivered"
            | "completed"
            | "delivered"
            | "collected"
            | "failed"
            | "cancelled"
            | "expired"
            | "interrupted_unknown"
    ) {
        return Ok(None);
    }
    Err(error("dispatch_task_state_changed"))
}

fn operation_outcome(
    tx: &Transaction<'_>,
    actor: Actor,
    operation: &str,
    state: &str,
    blocker: Option<&str>,
    now: i64,
) -> Result<(), MachineError> {
    captured_operation(tx, actor, operation)?;
    let count=tx.execute("UPDATE scheduler_operations SET state=?3,safe_blocker=?4 WHERE operation_id=?1 AND target_run_id=?2 AND state='prepared'",params![operation,actor.run.to_string(),state,blocker]).map_err(sql)?;
    if count != 1 {
        return Err(error("actor_operation_already_settled"));
    }
    event(
        tx,
        actor.owner,
        "actor_operation_outcome",
        &format!("{operation}\0{state}"),
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_short_wake_delivers_for_long_home_and_preserves_unknown_nodes() {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
        let root = std::env::temp_dir().join(format!("dg-wake-{}", Uuid::now_v7().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let account = root.join("long-account-home-".repeat(6));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&account)
            .unwrap();
        let home = crate::paths::DolgoraeHome::from_canonical_home(account.clone()).unwrap();
        let gateway =
            crate::gateway_socket::GatewaySocket::bind(&home, &root.join("g.sock")).unwrap();
        let instance = gateway.record().server_instance_id;
        assert!(
            home.root()
                .join(format!("rpc/wake-{}.sock", instance.simple()))
                .as_os_str()
                .len()
                >= 104
        );
        let (path, listener) = bind_wake_socket(home.root(), instance).unwrap();
        assert!(path.as_os_str().len() < 104);
        assert_eq!(wake_path(home.root()), Some(path.clone()));
        let original = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(original.mode() & 0o777, 0o600);
        assert!(bind_wake_socket(home.root(), instance).is_err());
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().ino(),
            original.ino()
        );
        let (other_path, other) = bind_wake_socket(home.root(), Uuid::now_v7()).unwrap();
        assert_ne!(path, other_path);
        assert_ne!(
            path,
            wake_socket_path(&account.join("other-installation"), instance)
        );
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let run = Uuid::now_v7();
        let workspace = "a".repeat(64);
        notify(&home.workspace_root(&workspace), run);
        let mut bytes = [0_u8; 2049];
        let length = listener.recv(&mut bytes).unwrap();
        let observed: Wake = serde_json::from_slice(&bytes[..length]).unwrap();
        assert_eq!(observed.workspace, workspace);
        assert_eq!(observed.run, run);
        other.set_nonblocking(true).unwrap();
        assert_eq!(
            other.recv(&mut bytes).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(listener);
        drop(other);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(other_path).unwrap();
        std::fs::write(&path, b"unknown endpoint must survive").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(bind_wake_socket(home.root(), instance).is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"unknown endpoint must survive"
        );
        std::fs::remove_file(path).unwrap();
        drop(gateway);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn policy() -> Policy {
        Policy {
            starvation_ms: 300_000,
            consecutive: 2,
            idle_ms: 600_000,
            activation_ms: 30_000,
            attempts: 3,
        }
    }
    fn item(source: &str, priority: u8, deadline: i64, sequence: i64) -> Candidate {
        Candidate {
            kind: "assignment".into(),
            id: Uuid::now_v7(),
            owner: Owner::Managed(Uuid::now_v7()),
            target: Uuid::now_v7(),
            source: source.into(),
            priority,
            deadline_ms: deadline,
            created_ms: 300_000,
            sequence,
            dependency_unblock: false,
        }
    }
    #[test]
    fn scheduler_order_discriminates_aging_priority_dependency_deadline_and_fifo() {
        let now = 600_000;
        let background = item("a", 2, 999_999, 99);
        let interactive = item("b", 0, 999_999, 1);
        // Exact starvation threshold is inclusive and dominates root priority.
        let mut young = interactive.clone();
        young.created_ms = 300_001;
        assert_eq!(
            select(vec![young.clone(), background.clone()], now, None, policy())
                .unwrap()
                .id,
            background.id
        );
        assert_eq!(
            select(vec![interactive.clone(), background], now, None, policy())
                .unwrap()
                .id,
            interactive.id
        );
        let mut boosted = item("c", 0, 999_999, 100);
        boosted.dependency_unblock = true;
        assert_eq!(
            select(
                vec![interactive.clone(), boosted.clone()],
                now,
                None,
                policy()
            )
            .unwrap()
            .id,
            boosted.id
        );
        let early = item("d", 0, 700_000, 999);
        assert_eq!(
            select(
                vec![interactive.clone(), early.clone()],
                now,
                None,
                policy()
            )
            .unwrap()
            .id,
            early.id
        );
        let fifo = item("e", 0, 999_999, 0);
        assert_eq!(
            select(vec![interactive, fifo.clone()], now, None, policy())
                .unwrap()
                .id,
            fifo.id
        );
    }
    #[test]
    fn scheduler_fairness_yields_only_to_an_eligible_equal_priority_source() {
        let a = item("a", 1, 900_000, 1);
        let b = item("b", 1, 900_000, 2);
        assert_eq!(
            select(
                vec![a.clone(), b.clone()],
                600_000,
                Some(("a", 1, 1)),
                policy()
            )
            .unwrap()
            .id,
            a.id
        );
        assert_eq!(
            select(vec![a.clone(), b], 600_000, Some(("a", 1, 2)), policy())
                .unwrap()
                .source,
            "b"
        );
        let low = item("b", 2, 700_000, 0);
        assert_eq!(
            select(vec![a.clone(), low], 600_000, Some(("a", 1, 3)), policy())
                .unwrap()
                .id,
            a.id
        );
        let mut young = item("b", 1, 700_000, 0);
        young.created_ms = 300_001;
        assert_eq!(
            select(vec![a.clone(), young], 600_000, Some(("a", 1, 3)), policy())
                .unwrap()
                .id,
            a.id
        );
    }

    #[test]
    fn scheduler_fairness_keeps_the_source_streak_across_priority_changes() {
        let now = 600_000;
        let mut a = item("a", 0, 700_000, 3);
        let mut b = item("b", 0, 800_000, 2);
        a.created_ms = now;
        b.created_ms = now - 1;
        // An older A1 can win the preceding selection through starvation.
        // Its priority does not erase A's two consecutive selections.
        assert_eq!(
            select(vec![a.clone(), b.clone()], now, Some(("a", 1, 2)), policy())
                .unwrap()
                .id,
            b.id
        );
        b.priority = 2;
        assert_eq!(
            select(vec![a.clone(), b.clone()], now, Some(("a", 1, 2)), policy())
                .unwrap()
                .id,
            a.id
        );
        a.created_ms = now - 300_000;
        b.priority = 0;
        assert_eq!(
            select(vec![a.clone(), b], now, Some(("a", 1, 2)), policy())
                .unwrap()
                .id,
            a.id
        );
    }
}
