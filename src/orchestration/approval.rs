//! Caller Primary's original Broker approvals projected into its private Run ledger.
use super::{
    OrchestrationStore, digest_value, idempotency_conflict, integrity,
    interaction_already_resolved, internal, validate_context,
};
use crate::interaction::{BROKER_APPROVAL_SCHEMA, BrokerApprovalBinding, BrokerApprovalRecord};
use crate::jcs::sha256_hex;
use crate::machine::MachineError;
use crate::run::RunStore;
use crate::session_policy::SessionPolicySnapshot;
use crate::workspace::SystemWorkspacePlatform;
use rusqlite::OptionalExtension as _;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};
use uuid::Uuid;

#[cfg(test)]
thread_local! {
    static AFTER_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn after_capture(hook: impl FnOnce() + 'static) {
    AFTER_CAPTURE.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

pub(crate) fn caller_root(manifest: &crate::run::RunManifest) -> bool {
    manifest.schema_version == 3
        && manifest.aggregate_binding.as_ref().is_some_and(|b| {
            b.aggregate_kind == crate::domain::AggregateKind::OrchestratedSession
                && b.member_kind == crate::run::AggregateMemberKind::Primary
                && b.aggregate_id == manifest.run_id
        })
}

pub(crate) fn recovery() -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "Broker approval observation requires its original durable Run binding",
        false,
        serde_json::json!({"required_action":"restore_state"}),
    )
}

fn timestamp(ms: i64) -> Result<String, MachineError> {
    let ms = u64::try_from(ms).map_err(|_| integrity("approval timestamp precedes epoch"))?;
    let at = UNIX_EPOCH
        .checked_add(Duration::from_millis(ms))
        .ok_or_else(|| integrity("approval timestamp overflow"))?;
    Ok(crate::ledger::format_system_timestamp(at))
}

fn original_bindings(
    store: &OrchestrationStore,
    run: Uuid,
    records: &[crate::audit::AuditRecord],
) -> Result<Vec<BrokerApprovalBinding>, MachineError> {
    let session = store.session(run)?;
    if !matches!(session.specialist_policy, SessionPolicySnapshot::Caller(_)) {
        return Ok(Vec::new());
    }
    store.validate_observed_event_chain(&store.connection, run)?;
    let mut result = Vec::new();
    for item in store.approval_interactions(run)? {
        let ctx = store.spawn_context(item.operation_id)?;
        validate_context(&ctx)?;
        if ctx.source_run_id != run {
            return Err(integrity("approval source Run differs"));
        }
        let source = records
            .iter()
            .find(|r| {
                r.kind() == crate::audit::AuditKind::TurnStarted
                    && r.payload_value()
                        .is_ok_and(|v| v["turn_id"] == ctx.source_turn_id)
            })
            .ok_or_else(recovery)?;
        let value = source.payload_value().map_err(internal)?;
        let thread = value["thread_id"].as_str().ok_or_else(recovery)?;
        let key = sha256_hex(
            format!(
                "dolgorae-primary-tool-v1\0{run}\0{run}\0{thread}\0{}\0{}",
                ctx.source_turn_id, ctx.source_tool_call_id
            )
            .as_bytes(),
        );
        if key != ctx.idempotency_key {
            return Err(integrity("approval native source identity differs"));
        }
        let request_sha: String = store
            .connection
            .query_row(
                "SELECT request_sha256 FROM brokered_spawn_operations WHERE operation_id=?1",
                [item.operation_id.to_string()],
                |r| r.get(0),
            )
            .map_err(internal)?;
        if digest_value(&item.request)? != request_sha {
            return Err(integrity("approval request identity differs"));
        }
        let resolution_key:Option<String>=store.connection.query_row("SELECT idempotency_key FROM brokered_approval_resolutions WHERE approval_request_id=?1",[item.approval_request_id.to_string()],|r|r.get(0)).optional().map_err(internal)?;
        let binding = BrokerApprovalBinding {
            session_id: run,
            operation_id: item.operation_id,
            approval_id: item.approval_request_id,
            source_turn_id: ctx.source_turn_id,
            source_call_id: ctx.source_tool_call_id,
            role_ref: item.request.role_ref,
            request_sha256: request_sha,
            opened_at: timestamp(item.created_at_ms)?,
            status: if item.decision.is_some() {
                "resolved"
            } else if item.operation_state == "awaiting_approval" && session.status == "active" {
                "pending"
            } else {
                "stale"
            }
            .into(),
            invalidated_at: if item.decision.is_none() && item.operation_state == "cancelled" {
                Some(timestamp(item.updated_at_ms)?)
            } else {
                None
            },
            decision: item.decision,
            resolution_key,
            resolution_receipt_id: item.resolution_receipt_id,
            resolved_at: item.resolved_at_ms.map(timestamp).transpose()?,
        };
        let marker = BrokerApprovalRecord {
            schema: BROKER_APPROVAL_SCHEMA.into(),
            phase: "binding".into(),
            binding: binding.clone(),
            intent: None,
            namespace: None,
            public_resolution: None,
        };
        marker.validate(run)?;
        result.push(binding);
    }
    Ok(result)
}

/// Pure complete-view read. SQL decisions are never silently exposed before publication.
pub(crate) fn checked_bindings(
    state_root: &Path,
    manifest: &crate::run::RunManifest,
    ledger: &crate::ledger::ObservedLedger,
) -> Result<Vec<BrokerApprovalBinding>, MachineError> {
    if !caller_root(manifest) {
        return Ok(Vec::new());
    }
    let store = OrchestrationStore::open_observer(state_root)?;
    let published = crate::interaction::broker_bindings(ledger.records(), manifest.run_id)?;
    let mut stable = None;
    for _ in 0..3 {
        let before = store.connection.observation_stamp()?;
        let observed = {
            let _snapshot = store.connection.unchecked_transaction().map_err(internal)?;
            original_bindings(&store, manifest.run_id, ledger.records())
        };
        if store.connection.observation_stamp()? != before {
            continue;
        }
        stable = Some(observed?);
        break;
    }
    let wanted = stable.ok_or_else(|| {
        MachineError::new(
            "RUN_STATE_CONFLICT",
            "approval observation moved; refresh the captured Run",
            false,
            serde_json::json!({"run_id":manifest.run_id,"operation":"capture_snapshot"}),
        )
    })?;
    if wanted.len() != published.len()
        || wanted
            .iter()
            .any(|v| published.get(&v.approval_id) != Some(v))
    {
        return Err(recovery());
    }

    for (native, _) in crate::interaction::durable_interactions_from_ledger(
        ledger,
        manifest.run_id,
        ledger.head(),
    )? {
        if wanted
            .iter()
            .any(|b| native["request_id"] == b.approval_id.to_string())
        {
            return Err(integrity("Broker and native approval identities collide"));
        }
    }
    Ok(wanted)
}

/// Owning Worker publication; SQL and Run ledger remain distinct recoverable stores.
pub(crate) fn publish(
    state_root: &Path,
    run: Uuid,
    ledger: &mut crate::ledger::Ledger,
    generation: u64,
) -> Result<(), MachineError> {
    let manifest = RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(run)?;
    if !caller_root(&manifest) {
        return Ok(());
    }
    let store = OrchestrationStore::open_observer(state_root)?;
    let records = ledger.durable_records().map_err(internal)?.to_vec();
    let wanted = {
        let _snapshot = store.connection.unchecked_transaction().map_err(internal)?;
        let bindings = original_bindings(&store, run, &records)?;
        #[cfg(test)]
        if let Some(hook) = AFTER_CAPTURE.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        bindings
    };
    store.connection.check_identity()?;
    let old = crate::interaction::broker_bindings(&records, run)?;
    let intents = crate::interaction::broker_intents(&records, run)?;
    if old
        .keys()
        .any(|id| !wanted.iter().any(|b| b.approval_id == *id))
    {
        return Err(integrity("published approval disappeared"));
    }
    for binding in wanted {
        if old.get(&binding.approval_id) == Some(&binding) {
            continue;
        }
        let intent = intents.get(&binding.approval_id).cloned();
        let (phase, intent, namespace, public_resolution) =
            if let (Some(receipt), Some(key), Some(prior)) = (
                binding.resolution_receipt_id,
                binding.resolution_key.as_ref(),
                intent,
            ) {
                let ns = prior.namespace.ok_or_else(recovery)?;
                let intent = prior.intent.ok_or_else(recovery)?;
                if &ns.idempotency_key != key {
                    return Err(integrity(
                        "approval decision key differs from original intent",
                    ));
                }
                let public = intent
                    .receipt(ns.clone(), receipt, false)
                    .map_err(internal)?;
                ("decision", Some(intent), Some(ns), Some(public))
            } else {
                ("binding", None, None, None)
            };
        let marker = BrokerApprovalRecord {
            schema: BROKER_APPROVAL_SCHEMA.into(),
            phase: phase.into(),
            binding,
            intent,
            namespace,
            public_resolution,
        };
        marker.validate(run)?;
        ledger
            .append_required_payload(crate::audit::AuditKind::TurnIntent, &marker, generation)
            .map_err(internal)?;
    }
    Ok(())
}

fn response_namespace(
    snapshot: &crate::snapshot::RunSnapshot,
    key: &str,
) -> Result<crate::operation::Namespace, MachineError> {
    crate::operation::Namespace::delegation(
        &snapshot.controller_authority()?.identity,
        &snapshot.manifest.workspace_id,
        Some(("run", snapshot.manifest.run_id.to_string().as_str())),
        "resolve_interaction",
        key,
    )
    .map_err(|_| integrity("approval response namespace is invalid"))
}

fn validate_response(
    request: &crate::interaction::BrokerApprovalResponse,
    run: Uuid,
) -> Result<(), MachineError> {
    request
        .intent
        .validate()
        .map_err(|_| integrity("approval response intent is invalid"))?;
    let body = &request.intent.body;
    if request.approval_id.get_version_num() != 7
        || request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 256
        || body.run_ref != run.to_string()
        || body.interaction_ref != request.approval_id.to_string()
        || body.decision != "accept_once"
        || body.protected_input_present
        || !body.granted_permissions.is_empty()
        || body.answers.len() != 1
        || body.answers[0].question_id != "specialist_approval"
        || body.answers[0].text.is_some()
        || body.answers[0].selected_labels
            != [if request.approved {
                "approve"
            } else {
                "reject"
            }]
    {
        return Err(integrity(
            "approval response does not match its closed question",
        ));
    }
    Ok(())
}

/// Exact accepted replay precedes fresh observation preconditions, but never authority.
pub(crate) fn replay_response(
    state_root: &Path,
    run: Uuid,
    request: &crate::interaction::BrokerApprovalResponse,
    controller: &crate::domain::ControllerIdentity,
    ledger: &crate::ledger::Ledger,
) -> Result<Option<Uuid>, MachineError> {
    validate_response(request, run)?;
    let manifest = RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(run)?;
    if !caller_root(&manifest) {
        return Err(integrity(
            "approval response is not owned by a Caller Primary",
        ));
    }
    let observed = crate::ledger::ObservedLedger::open_run(
        state_root,
        run,
        ledger.head().map_err(internal)?.sequence,
    )?;
    checked_bindings(state_root, &manifest, &observed)?;
    let namespace = crate::operation::Namespace::delegation(
        controller,
        &manifest.workspace_id,
        Some(("run", run.to_string().as_str())),
        "resolve_interaction",
        &request.idempotency_key,
    )
    .map_err(|_| integrity("approval response namespace"))?;
    for record in ledger.durable_records().map_err(internal)? {
        if let Some(marker) = crate::interaction::broker_record(record)?
            && marker.namespace.as_ref() == Some(&namespace)
            && marker.phase == "decision"
        {
            if marker.intent.as_ref() != Some(&request.intent)
                || marker.binding.approval_id != request.approval_id
                || marker.binding.decision.as_deref()
                    != Some(if request.approved {
                        "approve"
                    } else {
                        "reject"
                    })
            {
                return Err(idempotency_conflict(&request.idempotency_key));
            }
            return Ok(marker.binding.resolution_receipt_id);
        }
    }
    Ok(None)
}

/// Complete the owning Worker's original response without releasing uncertain
/// or accepted work. SQL reservation and ledger publication remain separate.
pub(crate) fn complete_response(
    state_root: &Path,
    run: Uuid,
    request: &crate::interaction::BrokerApprovalResponse,
    controller: &crate::domain::ControllerIdentity,
    result: &Result<super::BrokerApprovalResolution, MachineError>,
    ledger: &mut crate::ledger::Ledger,
    generation: u64,
) -> Result<Uuid, MachineError> {
    if let Err(error) = result {
        release_refused_response(
            state_root, run, request, controller, error, ledger, generation,
        )?;
    }
    publish(state_root, run, ledger, generation)?;
    replay_response(state_root, run, request, controller, ledger)?
        .ok_or_else(|| result.as_ref().err().cloned().unwrap_or_else(recovery))
}

fn release_refused_response(
    state_root: &Path,
    run: Uuid,
    request: &crate::interaction::BrokerApprovalResponse,
    controller: &crate::domain::ControllerIdentity,
    error: &MachineError,
    ledger: &mut crate::ledger::Ledger,
    generation: u64,
) -> Result<(), MachineError> {
    if !request.approved || error.code != "SPECIALIST_POLICY_DENIED" {
        return Ok(());
    }
    validate_response(request, run)?;
    let manifest = RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(run)?;
    if !caller_root(&manifest) {
        return Ok(());
    }
    let mut intents =
        crate::interaction::broker_intents(ledger.durable_records().map_err(internal)?, run)?;
    let Some(mut marker) = intents.remove(&request.approval_id) else {
        return Ok(());
    };
    let namespace = crate::operation::Namespace::delegation(
        controller,
        &manifest.workspace_id,
        Some(("run", &run.to_string())),
        "resolve_interaction",
        &request.idempotency_key,
    )
    .map_err(|_| integrity("approval refusal namespace"))?;
    if marker.intent.as_ref() != Some(&request.intent)
        || marker.namespace.as_ref() != Some(&namespace)
    {
        return Err(recovery());
    }
    let mut store = OrchestrationStore::open(state_root)?;
    store.authorize_primary(&store.spawn_context(marker.binding.operation_id)?)?;
    // Hold the Broker writer fence until the release is durable. A direct
    // resolver must not accept this approval between the proof and the marker.
    let transaction = store.transaction()?;
    let (state, child, decided): (String, Option<String>, bool) = transaction.query_row(
        "SELECT s.state,s.child_run_id,EXISTS(
           SELECT 1 FROM brokered_approval_resolutions r WHERE r.approval_request_id=s.approval_request_id)
         FROM brokered_spawn_operations s WHERE s.operation_id=?1 AND s.session_id=?2
           AND s.approval_request_id=?3",
        rusqlite::params![marker.binding.operation_id.to_string(), run.to_string(), request.approval_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).map_err(internal)?;
    if state == "awaiting_approval" && child.is_none() && !decided {
        marker.phase = "released".into();
        marker.validate(run)?;
        ledger
            .append_required_payload(crate::audit::AuditKind::TurnIntent, &marker, generation)
            .map_err(internal)?;
    }
    Ok(())
}

/// Runs on the owning drain after its unchanged Controller/Run admission checks.
pub(crate) fn prepare_response(
    state_root: &Path,
    run: Uuid,
    request: &crate::interaction::BrokerApprovalResponse,
    ledger: &mut crate::ledger::Ledger,
    generation: u64,
) -> Result<(), MachineError> {
    validate_response(request, run)?;
    if request.intent.body.turn_ref != request.expected_turn_id
        || request.intent.body.thread_generation != request.expected_thread_generation
    {
        return Err(integrity(
            "new approval intent differs from observed context",
        ));
    }
    let snapshot = crate::snapshot::RunSnapshot::load(state_root, run, 0)?;
    let current = snapshot
        .projection
        .active_turn_id
        .as_ref()
        .or(snapshot.projection.latest_turn_id.as_ref());
    if snapshot.stamp.run_state_revision != ledger.head().map_err(internal)?.sequence
        || snapshot.stamp.interaction_state_revision != request.expected_interaction_revision
        || current != Some(&request.expected_turn_id)
        || snapshot.captured_effective_policy.thread_generation
            != Some(request.expected_thread_generation)
        || matches!(
            snapshot.projection.lifecycle,
            crate::domain::RunLifecycle::Paused
                | crate::domain::RunLifecycle::Closed
                | crate::domain::RunLifecycle::StartFailed
                | crate::domain::RunLifecycle::OutcomeUnknown
        )
    {
        return Err(MachineError::new(
            "INTERACTION_STALE",
            "approval authority or observation changed",
            false,
            serde_json::json!({"run_id":run,"request_id":request.approval_id}),
        ));
    }
    let binding = snapshot
        .broker_approvals()?
        .iter()
        .find(|b| b.approval_id == request.approval_id)
        .ok_or_else(recovery)?
        .clone();
    if binding.status != "pending" {
        return Err(interaction_already_resolved(run, request.approval_id));
    }
    let store = OrchestrationStore::open_observer(state_root)?;
    store.authorize_primary(&store.spawn_context(binding.operation_id)?)?;
    let namespace = response_namespace(&snapshot, &request.idempotency_key)?;
    let intents =
        crate::interaction::broker_intents(ledger.durable_records().map_err(internal)?, run)?;
    if let Some(marker) = intents.get(&request.approval_id) {
        if marker.namespace.as_ref() != Some(&namespace)
            || marker.intent.as_ref() != Some(&request.intent)
        {
            return Err(interaction_already_resolved(run, request.approval_id));
        }
        // An unresolved response cannot be replayed as a new effect.
        return Err(recovery());
    }
    let marker = BrokerApprovalRecord {
        schema: BROKER_APPROVAL_SCHEMA.into(),
        phase: "intent".into(),
        binding,
        intent: Some(request.intent.clone()),
        namespace: Some(namespace),
        public_resolution: None,
    };
    marker.validate(run)?;
    ledger
        .append_required_payload(crate::audit::AuditKind::TurnIntent, &marker, generation)
        .map_err(internal)?;
    Ok(())
}
