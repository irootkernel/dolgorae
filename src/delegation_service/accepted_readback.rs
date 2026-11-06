//! Checked accepted-task result and outcome reads within the parent snapshot.

use super::{DelegationScope, SettlementOwner, complete_result_unavailable, verify_result_source};
use crate::delegation_identity::OperationReceipt;
use crate::delegation_service::research_merge;
use crate::engagement::{self, EngagementStore};
use crate::jcs::sha256_hex;
use crate::machine::MachineError;
use serde_json::{Value, json};
use uuid::Uuid;

/// Read only the original ordinary task's settled identity and byte reference.
pub(super) fn ordinary_lookup_result(
    scope: &DelegationScope<'_>,
    store: &EngagementStore,
    receipt: &OperationReceipt,
) -> Result<Option<(crate::delegation_identity::BytesRef, String)>, MachineError> {
    use rusqlite::OptionalExtension as _;
    let refused = || complete_result_unavailable("lookup_accepted");
    let external = match receipt.namespace.operation_kind.as_str() {
        "assign_external_specialist_task" => true,
        "assign_specialist_task" => false,
        "assign_synthesis" => match receipt.namespace.scope_kind.as_deref() {
            Some("external_engagement") => true,
            Some("orchestrated_session") => false,
            _ => return Err(refused()),
        },
        _ => return Ok(None),
    };
    let task = receipt
        .accepted_subjects
        .task_ref
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(refused)?;
    let member = receipt
        .accepted_subjects
        .member_ref
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(refused)?;
    if receipt.accepted_subjects.run_ref.as_deref() != Some(member.to_string().as_str()) {
        return Err(refused());
    }
    let aggregate = receipt
        .namespace
        .scope_ref
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(refused)?;
    let connection = store.connection();
    let (owner, state, accepted, content, recorded) = if external {
        if receipt.namespace.scope_kind.as_deref() != Some("external_engagement") {
            return Err(refused());
        }
        let (run, state, digest, artifact): (String, String, Option<String>, Option<String>) = connection.query_row(
            "SELECT specialist_run_id,state,result_sha256,artifact_id FROM tasks WHERE engagement_id=?1 AND task_id=?2",
            rusqlite::params![aggregate.to_string(),task.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).map_err(|_| refused())?;
        if run != member.to_string() {
            return Err(refused());
        }
        let accepted = engagement::external_task_identity_metadata_on(connection, aggregate, task)?
            .ok_or_else(refused)?;
        let content = if let Some(artifact) = artifact {
            let linked: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM artifacts WHERE artifact_id=?1 AND engagement_id=?2 AND task_id=?3 AND specialist_run_id=?4 AND result_sha256=?5)",
                rusqlite::params![artifact,aggregate.to_string(),task.to_string(),member.to_string(),digest], |r|r.get(0),
            ).map_err(|_|refused())?;
            if !linked {
                return Err(refused());
            }
            crate::storage_observation::content_reference(
                connection,
                "external_artifact",
                &artifact,
                "json",
            )?
        } else {
            None
        };
        (
            SettlementOwner::External {
                engagement_id: aggregate,
            },
            state,
            accepted,
            content,
            digest,
        )
    } else {
        if receipt.namespace.scope_kind.as_deref() != Some("orchestrated_session") {
            return Err(refused());
        }
        let facts = crate::orchestration::successor_task(connection, aggregate, task)?
            .ok_or_else(refused)?;
        if facts.member_ref != member.to_string() {
            return Err(refused());
        }
        let recorded: Option<String> = connection
            .query_row(
                "SELECT accepted_request_sha256 FROM brokered_task_acceptances WHERE task_id=?1",
                [task.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| refused())?;
        if recorded.as_deref() != Some(sha256_hex(facts.accepted_request_json.as_bytes()).as_str())
        {
            return Err(refused());
        }
        let accepted =
            crate::successor_capture::retained_accepted_task(&facts.accepted_request_json)?;
        let marker: Value = serde_json::from_str(&facts.request_json).map_err(|_| refused())?;
        if marker["operation_ref"] != receipt.operation_ref
            || marker["member_ref"] != member.to_string()
            || marker["accepted_task_digest"] != accepted.digest()?
            || marker["execution_intent"] != accepted.execution_intent
        {
            return Err(refused());
        }
        let content = crate::storage_observation::content_reference(
            connection,
            "successor_task_result",
            &task.to_string(),
            "complete_result",
        )?;
        (
            SettlementOwner::Managed {
                session_id: aggregate,
            },
            facts.state,
            accepted,
            content,
            facts.result_sha256,
        )
    };
    if receipt.namespace.operation_kind == "assign_synthesis" {
        research_merge::verify_task_receipt(store, receipt, &accepted)?;
    }
    if !matches!(
        state.as_str(),
        "completed_not_delivered" | "completed" | "delivered" | "failed" | "cancelled" | "expired"
    ) {
        if content.is_some() || recorded.is_some() {
            return Err(refused());
        }
        return Ok(None);
    }
    let body_free = crate::delegation_identity::CompleteResult {
        schema: crate::delegation_identity::COMPLETE_RESULT_SCHEMA.into(),
        source: crate::delegation_identity::ResultSource {
            scope_kind: owner.scope_kind().into(),
            scope_ref: aggregate.to_string(),
            worker_ref: member.to_string(),
            task_ref: task.to_string(),
            role_snapshot_digest: accepted.role_snapshot_digest.clone(),
            agent_configuration_digest: accepted.agent_configuration_digest.clone(),
            accepted_task_digest: accepted.digest()?,
        },
        terminal_outcome: "expired".into(),
        safe_failure_code: Some("OPERATION_TIMEOUT".into()),
        final_response: None,
        structured_output_schema: None,
        structured_output: None,
        changes: Vec::new(),
        attachments: Vec::new(),
    };
    let identity = crate::result_component_artifact::Publication::retained_lookup_identity(
        connection, scope.view, member, task, &body_free,
    )?;
    let (content, recorded) = match (content, recorded) {
        (None, None) if identity.is_none() => return Ok(None),
        (Some(content), Some(recorded)) => (content, recorded),
        _ => return Err(refused()),
    };
    if content.sha256 != recorded {
        return Err(refused());
    }
    let result = match identity {
        Some(result) => result,
        None => {
            let bytes = crate::storage_observation::read_content(
                connection,
                &content.owner_kind,
                &content.owner_id,
                &content.representation,
            )?
            .ok_or_else(refused)?;
            let parsed = crate::jcs::parse(std::str::from_utf8(&bytes).map_err(|_| refused())?)
                .map_err(|_| refused())?;
            let canonical = crate::jcs::canonicalize(&parsed).map_err(|_| refused())?;
            let value: Value = serde_json::from_slice(&canonical).map_err(|_| refused())?;
            if value["schema"] == "dolgorae.complete-result-unavailable/v1" {
                if value
                    != json!({"schema":"dolgorae.complete-result-unavailable/v1","task_ref":task,"reason":"complete_result_unavailable"})
                {
                    return Err(refused());
                }
                return Ok(None);
            }
            serde_json::from_value(value).map_err(|_| refused())?
        }
    };
    verify_result_source(
        &result,
        owner,
        member,
        task,
        &accepted.digest()?,
        &state,
        "lookup_accepted",
    )?;
    if result.source.role_snapshot_digest != accepted.role_snapshot_digest
        || result.source.agent_configuration_digest != accepted.agent_configuration_digest
    {
        return Err(refused());
    }
    let bytes = crate::operation::canonical(&serde_json::to_value(&result).map_err(|_| refused())?)
        .map_err(|_| refused())?;
    let reference = crate::delegation_identity::BytesRef::from_bytes(&bytes);
    if reference.sha256 != format!("sha256:{}", content.sha256)
        || reference.byte_length != content.byte_length
    {
        return Err(refused());
    }
    Ok(Some((reference, result.digest()?)))
}

/// Original synthesis execution facts share the lookup's read snapshot.
fn synthesis_outcome(
    store: &EngagementStore,
    receipt: &OperationReceipt,
) -> Result<&'static str, MachineError> {
    use rusqlite::OptionalExtension as _;
    let refused = || complete_result_unavailable("lookup_accepted");
    let external = receipt.namespace.scope_kind.as_deref() == Some("external_engagement");
    let operation = if external {
        "assign_external_specialist_task"
    } else {
        "assign_specialist_task"
    };
    let outcome = current_outcome(store, operation, receipt)?;
    let task = receipt
        .accepted_subjects
        .task_ref
        .as_deref()
        .ok_or_else(refused)?;
    let member = receipt
        .accepted_subjects
        .member_ref
        .as_deref()
        .ok_or_else(refused)?;
    let turn: Option<String> = store
        .connection()
        .query_row(
            if external {
                "SELECT turn_id FROM tasks WHERE task_id=?1"
            } else {
                "SELECT target_turn_id FROM brokered_tasks WHERE task_id=?1"
            },
            [task],
            |row| row.get(0),
        )
        .map_err(|_| refused())?;
    let claim: Option<(String,String,String,String,String,Option<String>)> = store.connection().query_row(
        "SELECT kind,target_run_id,scope_kind,scope_ref,phase,turn_id FROM scheduler_claims WHERE work_ref=?1",
        [task], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).optional().map_err(|_| refused())?;
    let Some((kind, run, scope_kind, scope_ref, phase, claimed_turn)) = claim else {
        return Ok(outcome);
    };
    if kind != "assignment"
        || run != member
        || Some(scope_kind.as_str()) != receipt.namespace.scope_kind.as_deref()
        || Some(scope_ref.as_str()) != receipt.namespace.scope_ref.as_deref()
        || !matches!(
            phase.as_str(),
            "claimed" | "dispatching" | "accepted" | "unknown" | "refused"
        )
        || (phase == "accepted" && claimed_turn.as_deref().is_none_or(str::is_empty))
        || (phase != "accepted" && claimed_turn.is_some())
        || turn
            .as_ref()
            .zip(claimed_turn.as_ref())
            .is_some_and(|(a, b)| a != b)
    {
        return Err(refused());
    }
    if matches!(outcome, "completed" | "failed" | "cancelled" | "expired") {
        return Ok(outcome);
    }
    if phase == "unknown" && turn.is_none() {
        if !matches!(outcome, "running" | "unknown") {
            return Err(refused());
        }
        return Ok("unknown");
    }
    Ok(outcome)
}

/// The closed `lookup_accepted.current_outcome` derivation.
pub(super) fn current_outcome(
    store: &EngagementStore,
    operation_kind: &str,
    receipt: &OperationReceipt,
) -> Result<&'static str, MachineError> {
    if matches!(
        operation_kind,
        "assign_external_specialist_task" | "assign_specialist_task"
    ) && receipt.namespace.operation_kind != "assign_synthesis"
    {
        return accepted_assignment_outcome(store, receipt);
    }
    // Synthesis keeps its own Task/Turn observation and established precedence.
    match operation_kind {
        // A group receipt accepts the assignment set, not a single execution.
        // Child outcomes and complete coverage belong to await/collect.
        "assign_group" => Ok("pending"),
        "assign_synthesis" => synthesis_outcome(store, receipt),
        // Install and open have no delegated execution to observe.
        "install_provisioning" | "open_external_engagement" => Ok("completed"),
        "assign_external_specialist_task" => {
            let task_ref = receipt
                .accepted_subjects
                .task_ref
                .as_deref()
                .ok_or_else(|| {
                    MachineError::invalid_argument("receipt", "the receipt names no task")
                })?;
            let task_id = Uuid::parse_str(task_ref)
                .map_err(|_| MachineError::invalid_argument("receipt", "invalid task identity"))?;
            let engagement_id = receipt
                .namespace
                .scope_ref
                .as_deref()
                .and_then(|scope| Uuid::parse_str(scope).ok())
                .ok_or_else(|| {
                    MachineError::invalid_argument("receipt", "invalid engagement scope")
                })?;
            let state: String = store
                .connection()
                .query_row(
                    "SELECT state FROM tasks WHERE engagement_id=?1 AND task_id=?2",
                    rusqlite::params![engagement_id.to_string(), task_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(|_| complete_result_unavailable("lookup_accepted"))?;
            Ok(match state.as_str() {
                "accepted" | "queued" | "claimed" => "pending",
                "dispatching" | "running" => "running",
                "completed_not_delivered" | "delivered" => "completed",
                "failed" => "failed",
                "cancelled" => "cancelled",
                "expired" => "expired",
                _ => "unknown",
            })
        }
        "hire_external_specialist" => {
            let member_ref = receipt
                .accepted_subjects
                .member_ref
                .as_deref()
                .ok_or_else(|| {
                    MachineError::invalid_argument("receipt", "the receipt names no member")
                })?;
            let member_id = Uuid::parse_str(member_ref).map_err(|_| {
                MachineError::invalid_argument("receipt", "invalid member identity")
            })?;
            let engagement_id = receipt
                .namespace
                .scope_ref
                .as_deref()
                .and_then(|scope| Uuid::parse_str(scope).ok())
                .ok_or_else(|| {
                    MachineError::invalid_argument("receipt", "invalid engagement scope")
                })?;
            let (state, residency) = store.external_member_state(engagement_id, member_id)?;
            Ok(match (state.as_str(), residency.as_str()) {
                ("provisioning", _) => "pending",
                ("active" | "degraded" | "releasing", "recovering") | ("unknown", _) => "unknown",
                ("degraded", "unavailable") => "failed",
                ("degraded", _) => "unknown",
                _ => "completed",
            })
        }
        "spawn_specialist" => {
            // Bind the original spawn before deriving its current member outcome.
            // A failed publication may have no manifest, including retained rows.
            let member_ref = receipt
                .accepted_subjects
                .member_ref
                .as_deref()
                .ok_or_else(|| {
                    MachineError::invalid_argument("receipt", "the receipt names no member")
                })?;
            let member_id = Uuid::parse_str(member_ref).map_err(|_| {
                MachineError::invalid_argument("receipt", "invalid member identity")
            })?;
            let session_id = receipt
                .namespace
                .scope_ref
                .as_deref()
                .and_then(|scope| Uuid::parse_str(scope).ok())
                .ok_or_else(|| {
                    MachineError::invalid_argument("receipt", "invalid session scope")
                })?;
            let facts =
                crate::orchestration::successor_member(store.connection(), session_id, member_id)?
                    .ok_or_else(|| {
                        MachineError::invalid_argument("receipt", "the receipt names no member")
                    })?;
            let unbound = || crate::scheduler::error("accepted_identity_changed");
            if facts.spawn_operation_id.to_string() != receipt.operation_ref {
                return Err(unbound());
            }
            let spawn_state: String = store
                .connection()
                .query_row(
                    "SELECT state FROM brokered_spawn_operations
                 WHERE operation_id=?1 AND session_id=?2 AND child_run_id=?3",
                    rusqlite::params![
                        receipt.operation_ref,
                        session_id.to_string(),
                        member_id.to_string()
                    ],
                    |row| row.get(0),
                )
                .map_err(|_| unbound())?;
            if matches!(facts.membership_state.as_str(), "retired" | "released") {
                return Ok("completed");
            }
            if spawn_state == "requested" {
                return Ok(if facts.membership_state == "provisioning" {
                    "pending"
                } else {
                    "unknown"
                });
            }
            if spawn_state == "recovery_required" {
                return Ok("unknown");
            }
            Ok(
                match (
                    facts.membership_state.as_str(),
                    facts.actor_residency.as_str(),
                ) {
                    ("provisioning", _) => "pending",
                    ("active", "recovering") | ("degraded", "recovering") | ("unknown", _) => {
                        "unknown"
                    }
                    ("degraded", "unavailable") => "failed",
                    ("degraded", _) => "unknown",
                    _ => "completed",
                },
            )
        }
        "assign_specialist_task" => {
            // The managed task's own Broker row is the execution fact.
            let task_ref = receipt
                .accepted_subjects
                .task_ref
                .as_deref()
                .ok_or_else(|| {
                    MachineError::invalid_argument("receipt", "the receipt names no task")
                })?;
            let task_id = Uuid::parse_str(task_ref)
                .map_err(|_| MachineError::invalid_argument("receipt", "invalid task identity"))?;
            let session_id = receipt
                .namespace
                .scope_ref
                .as_deref()
                .and_then(|scope| Uuid::parse_str(scope).ok())
                .ok_or_else(|| {
                    MachineError::invalid_argument("receipt", "invalid session scope")
                })?;
            let facts =
                crate::orchestration::successor_task(store.connection(), session_id, task_id)?
                    .ok_or_else(|| {
                        MachineError::invalid_argument("receipt", "the receipt names no task")
                    })?;
            Ok(match facts.state.as_str() {
                "accepted" | "queued" | "claimed" => "pending",
                "dispatching" | "running" => "running",
                "completed_not_delivered" | "delivered" => "completed",
                "failed" => "failed",
                "cancelled" => "cancelled",
                "expired" => "expired",
                _ => "unknown",
            })
        }
        _ => Ok("completed"),
    }
}

/// Outcome metadata shares the receipt's pinned read transaction. No source
/// content, live port, schema installation or dispatch authority enters lookup.
pub(super) fn accepted_assignment_outcome(
    store: &EngagementStore,
    receipt: &OperationReceipt,
) -> Result<&'static str, MachineError> {
    use rusqlite::OptionalExtension as _;
    let c = store.connection();
    let refused = || crate::scheduler::error("accepted_identity_changed");
    let scope = receipt
        .namespace
        .scope_ref
        .as_deref()
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(refused)?;
    let task = receipt
        .accepted_subjects
        .task_ref
        .as_deref()
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(refused)?;
    let member = receipt
        .accepted_subjects
        .member_ref
        .as_deref()
        .ok_or_else(refused)?;
    let managed = receipt.namespace.operation_kind == "assign_specialist_task";
    let expected_scope = if managed {
        "orchestrated_session"
    } else {
        "external_engagement"
    };
    if receipt.namespace.scope_kind.as_deref() != Some(expected_scope) {
        return Err(refused());
    }
    let (state, turn, identity_present) = if managed {
        let facts = crate::orchestration::successor_task(c, scope, task)?.ok_or_else(refused)?;
        let accepted =
            crate::successor_capture::retained_accepted_task(&facts.accepted_request_json)?;
        let marker: Value = serde_json::from_str(&facts.request_json).map_err(|_| refused())?;
        let digest: String = c
            .query_row(
                "SELECT accepted_request_sha256 FROM brokered_task_acceptances WHERE task_id=?1",
                [task.to_string()],
                |r| r.get(0),
            )
            .map_err(crate::scheduler::sql)?;
        if facts.member_ref != member
            || crate::jcs::sha256_hex(facts.accepted_request_json.as_bytes()) != digest
            || marker["execution_intent"].as_str() != Some(accepted.execution_intent.as_str())
            || marker["schema"] != "dolgorae.successor-task/v1"
        {
            return Err(refused());
        }
        (facts.state, facts.target_turn_id, true)
    } else {
        let facts = store.external_task(scope, task)?;
        if facts.specialist_run_id.to_string() != member {
            return Err(refused());
        }
        let metadata = engagement::external_task_identity_metadata_on(c, scope, task)?;
        if let Some(accepted) = &metadata {
            let (intent, _) = engagement::external_task_dispatch_facts_on(c, scope, task)?;
            if intent != accepted.execution_intent {
                return Err(refused());
            }
        }
        let identity_present = if let Some(accepted) = &metadata {
            let (objective, reference): (String, Option<String>) = c
                .query_row(
                    "SELECT objective_sha256,external_request_ref_json FROM tasks WHERE task_id=?1",
                    [task.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(crate::scheduler::sql)?;
            let digest = accepted.digest()?;
            if objective != digest {
                return Err(refused());
            }
            let reference: Option<Value> = reference
                .map(|v| serde_json::from_str(&v))
                .transpose()
                .map_err(|_| refused())?;
            reference
                .as_ref()
                .and_then(|v| v["accepted_task_digest"].as_str())
                == Some(digest.as_str())
        } else {
            false
        };
        (facts.state, facts.turn_id, identity_present)
    };
    crate::scheduler::validate_schema(c)?;
    let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='scheduler_claims')", [], |r| r.get(0)).map_err(crate::scheduler::sql)?;
    type ClaimRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let claim: Option<ClaimRow> = if exists {
        c.query_row("SELECT claim_id,kind,target_run_id,scope_kind,scope_ref,phase,turn_id,safe_blocker FROM scheduler_claims WHERE work_ref=?1",[task.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional().map_err(crate::scheduler::sql)?
    } else {
        None
    };
    let mut unknown = false;
    if let Some((id, kind, target, scope_kind, scope_ref, phase, claim_turn, blocker)) = claim {
        if Uuid::parse_str(&id).is_err()
            || kind != "assignment"
            || target != member
            || scope_kind != expected_scope
            || scope_ref != scope.to_string()
            || !matches!(
                phase.as_str(),
                "claimed" | "dispatching" | "accepted" | "unknown" | "refused"
            )
            || (phase == "accepted" && claim_turn.is_none())
            || (phase == "unknown" && blocker.as_deref() != Some("dispatch_outcome_unknown"))
            || (claim_turn.is_some() && turn.is_none())
            || (claim_turn.is_some() && turn.is_some() && claim_turn != turn)
        {
            return Err(crate::scheduler::error("claim_identity_invalid"));
        }
        unknown = state == "dispatching"
            && turn.is_none()
            && phase == "unknown"
            && claim_turn.is_none()
            && blocker.as_deref() == Some("dispatch_outcome_unknown");
        let terminal = matches!(
            state.as_str(),
            "completed_not_delivered" | "delivered" | "failed" | "cancelled" | "expired"
        );
        if unknown && !identity_present {
            return Err(refused());
        }
        if !terminal && phase == "unknown" && turn.is_none() && !unknown {
            return Err(crate::scheduler::error("claim_identity_invalid"));
        }
    }
    Ok(match state.as_str() {
        "completed_not_delivered" | "delivered" => "completed",
        "failed" => "failed",
        "cancelled" => "cancelled",
        "expired" => "expired",
        "accepted" | "queued" | "claimed" => "pending",
        "dispatching" if unknown => "unknown",
        "dispatching" | "running" => "running",
        _ => "unknown",
    })
}
