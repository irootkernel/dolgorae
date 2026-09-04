//! Checked private facade for reusable externally controlled Specialists.

use crate::controller::{CredentialCarrier, binding_from_carrier, carrier_from_options};
use crate::darwin::DarwinSystem;
use crate::domain::{AggregateKind, RunLifecycle};
use crate::engagement::{
    EngagementStore, ExternalTaskRequest, ExternalTaskSnapshot, RuntimeOutcome,
};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::run::{
    AggregateBinding, AggregateMemberKind, RunStore, agent_configuration_digest, run_root,
};
use crate::semantic::{
    ExternalAgentConfigurationInput, ExternalSpecialistStartContext, acquire_external_writer,
    ensure_external_specialist_worker, prepare_external_specialist, release_external_writer,
    start_external_specialist_run,
};
use crate::turn::FinalResponse;
use crate::worker::{ControlRequestV1, ControlResponseV1, TurnControlRequest, call_run_worker};
use crate::workspace::{
    SystemWorkspacePlatform, WorkspaceMode, WorkspaceService, WorkspaceView,
    add_detached_git_worktree, capture_git_worktree_patch, is_registered_git_worktree,
    isolated_specialist_root, remove_git_worktree, verify_secure_directory, verify_secure_file,
};
use base64::Engine as _;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal as _, Read as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_REQUEST_BYTES: u64 = 1_048_576;

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct OpaqueRef {
    namespace: String,
    kind: String,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    OpenExternalEngagement {
        external_controller_ref: OpaqueRef,
        #[serde(deserialize_with = "deserialize_required_option")]
        label: Option<String>,
        idempotency_key: String,
    },
    GetExternalEngagement {
        engagement_id: Uuid,
    },
    HireExternalSpecialist {
        engagement_id: Uuid,
        role_ref: String,
        agent_configuration: ExternalAgentConfigurationInput,
        objective: String,
        requested_access: String,
        idempotency_key: String,
    },
    AssignExternalSpecialistTask {
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        external_request_ref: OpaqueRef,
        objective: String,
        context_refs: Vec<Uuid>,
        expected_output: Vec<String>,
        execution_intent: String,
        deadline_seconds: u64,
        idempotency_key: String,
    },
    AwaitExternalSpecialistTasks {
        engagement_id: Uuid,
        task_ids: Vec<Uuid>,
        return_when: String,
        transport_wait_seconds: u64,
    },
    CollectExternalSpecialistResults {
        engagement_id: Uuid,
        after_sequence: u64,
        limit: usize,
    },
    CancelExternalSpecialistTask {
        engagement_id: Uuid,
        task_id: Uuid,
        reason: String,
        idempotency_key: String,
    },
    ReleaseExternalSpecialist {
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        reason: String,
        idempotency_key: String,
    },
    CloseExternalEngagement {
        engagement_id: Uuid,
        mode: String,
        reason: String,
        idempotency_key: String,
    },
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

impl Request {
    fn engagement_id(&self) -> Option<Uuid> {
        match self {
            Self::OpenExternalEngagement { .. } => None,
            Self::GetExternalEngagement { engagement_id }
            | Self::HireExternalSpecialist { engagement_id, .. }
            | Self::AssignExternalSpecialistTask { engagement_id, .. }
            | Self::AwaitExternalSpecialistTasks { engagement_id, .. }
            | Self::CollectExternalSpecialistResults { engagement_id, .. }
            | Self::CancelExternalSpecialistTask { engagement_id, .. }
            | Self::ReleaseExternalSpecialist { engagement_id, .. }
            | Self::CloseExternalEngagement { engagement_id, .. } => Some(*engagement_id),
        }
    }

    fn validate(&self) -> Result<(), MachineError> {
        match self {
            Self::OpenExternalEngagement {
                external_controller_ref,
                label,
                idempotency_key,
            } => {
                external_controller_ref.validate("external_controller_ref")?;
                if label
                    .as_ref()
                    .is_some_and(|value| value.len() > 256 || value.contains('\0'))
                {
                    return Err(MachineError::invalid_argument(
                        "label",
                        "label exceeds the checked bound",
                    ));
                }
                bounded(idempotency_key, 256, "idempotency_key")
            }
            Self::GetExternalEngagement { engagement_id } => uuid7(*engagement_id, "engagement_id"),
            Self::HireExternalSpecialist {
                engagement_id,
                role_ref,
                objective,
                requested_access,
                idempotency_key,
                ..
            } => {
                uuid7(*engagement_id, "engagement_id")?;
                if role_ref.is_empty()
                    || role_ref.len() > 64
                    || !role_ref.bytes().enumerate().all(|(index, byte)| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
                    })
                {
                    return Err(MachineError::invalid_argument(
                        "role_ref",
                        "role reference does not match the checked pattern",
                    ));
                }
                bounded(objective, 65_536, "objective")?;
                if !matches!(
                    requested_access.as_str(),
                    "read_only" | "isolated_write" | "canonical_workspace_write"
                ) {
                    return Err(MachineError::invalid_argument(
                        "requested_access",
                        "unsupported access",
                    ));
                }
                bounded(idempotency_key, 256, "idempotency_key")
            }
            Self::AssignExternalSpecialistTask {
                engagement_id,
                specialist_run_id,
                external_request_ref,
                objective,
                context_refs,
                expected_output,
                execution_intent,
                deadline_seconds,
                idempotency_key,
            } => {
                uuid7(*engagement_id, "engagement_id")?;
                uuid7(*specialist_run_id, "specialist_run_id")?;
                external_request_ref.validate("external_request_ref")?;
                bounded(objective, 65_536, "objective")?;
                unique_uuid7(context_refs, 64, "context_refs")?;
                if expected_output.is_empty() || expected_output.len() > 32 {
                    return Err(MachineError::invalid_argument(
                        "expected_output",
                        "expected output count is outside the checked bound",
                    ));
                }
                for value in expected_output {
                    bounded(value, 512, "expected_output")?;
                }
                if !matches!(
                    execution_intent.as_str(),
                    "read_only" | "isolated_write" | "canonical_workspace_write"
                ) {
                    return Err(MachineError::invalid_argument(
                        "execution_intent",
                        "unsupported execution intent",
                    ));
                }
                if !(1..=86_400).contains(deadline_seconds) {
                    return Err(MachineError::invalid_argument(
                        "deadline_seconds",
                        "deadline is outside the checked bound",
                    ));
                }
                bounded(idempotency_key, 256, "idempotency_key")
            }
            Self::AwaitExternalSpecialistTasks {
                engagement_id,
                task_ids,
                return_when,
                transport_wait_seconds,
            } => {
                uuid7(*engagement_id, "engagement_id")?;
                unique_uuid7(task_ids, 32, "task_ids")?;
                if task_ids.is_empty()
                    || !matches!(return_when.as_str(), "any" | "all")
                    || !(1..=60).contains(transport_wait_seconds)
                {
                    return Err(MachineError::invalid_argument(
                        "await",
                        "task wait does not match facade v1",
                    ));
                }
                Ok(())
            }
            Self::CollectExternalSpecialistResults {
                engagement_id,
                limit,
                ..
            } => {
                uuid7(*engagement_id, "engagement_id")?;
                if !(1..=100).contains(limit) {
                    return Err(MachineError::invalid_argument(
                        "limit",
                        "limit is outside the checked bound",
                    ));
                }
                Ok(())
            }
            Self::CancelExternalSpecialistTask {
                engagement_id,
                task_id,
                reason,
                idempotency_key,
            } => {
                uuid7(*engagement_id, "engagement_id")?;
                uuid7(*task_id, "task_id")?;
                bounded(reason, 1024, "reason")?;
                bounded(idempotency_key, 256, "idempotency_key")
            }
            Self::ReleaseExternalSpecialist {
                engagement_id,
                specialist_run_id,
                reason,
                idempotency_key,
            } => {
                uuid7(*engagement_id, "engagement_id")?;
                uuid7(*specialist_run_id, "specialist_run_id")?;
                bounded(reason, 1024, "reason")?;
                bounded(idempotency_key, 256, "idempotency_key")
            }
            Self::CloseExternalEngagement {
                engagement_id,
                mode,
                reason,
                idempotency_key,
            } => {
                uuid7(*engagement_id, "engagement_id")?;
                if !matches!(mode.as_str(), "complete" | "abort") {
                    return Err(MachineError::invalid_argument(
                        "mode",
                        "unsupported close mode",
                    ));
                }
                bounded(reason, 1024, "reason")?;
                bounded(idempotency_key, 256, "idempotency_key")
            }
        }
    }
}

impl OpaqueRef {
    fn validate(&self, field: &str) -> Result<(), MachineError> {
        for value in [&self.namespace, &self.kind] {
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(MachineError::invalid_argument(
                    field,
                    "opaque reference namespace and kind must be printable and bounded",
                ));
            }
        }
        if self.id.is_empty() || self.id.len() > 512 || self.id.contains('\0') {
            return Err(MachineError::invalid_argument(
                field,
                "opaque reference id is outside the checked bound",
            ));
        }
        Ok(())
    }
}

pub fn execute_cli(arguments: &[OsString]) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(arguments, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?
        .ok_or_else(|| MachineError::invalid_argument("--workspace", "workspace is required"))?;
    let view = WorkspaceService::system()?.discover(Some(&workspace))?;
    let state_root = DolgoraeHome::system()?.workspace_root(&view.workspace_id);
    let request = read_request(arguments)?;
    request.validate()?;
    let has_new_controller = option(arguments, "--new-controller-file")?.is_some()
        || option(arguments, "--new-controller-fd")?.is_some();
    if has_new_controller != matches!(&request, Request::HireExternalSpecialist { .. }) {
        return Err(MachineError::invalid_argument(
            "--new-controller-file",
            "exactly one new Specialist Controller carrier is required only for hire_external_specialist",
        ));
    }
    let owner = carrier_from_options(arguments, "--controller-file", "--controller-fd")?;
    let database = EngagementStore::workspace_database_path(&state_root);
    let mut store = EngagementStore::open(&database)?;
    let _operation_lock = request
        .engagement_id()
        .map(|engagement_id| EngagementOperationLock::acquire(&state_root, engagement_id))
        .transpose()?;

    match request {
        Request::OpenExternalEngagement {
            external_controller_ref,
            label,
            idempotency_key,
        } => {
            let binding = binding_from_carrier(&owner, 1)?;
            let external_controller_ref =
                serde_json::to_value(external_controller_ref).map_err(internal)?;
            let snapshot = store.open_external_engagement(
                &view.workspace_id,
                &binding,
                &external_controller_ref,
                label.as_deref(),
                &idempotency_key,
            )?;
            Ok(
                json!({"operation":"open_external_engagement_result","engagement_id":snapshot.engagement_id,"state":snapshot.state}),
            )
        }
        Request::GetExternalEngagement { engagement_id } => {
            authorize(
                &store,
                &view.workspace_id,
                engagement_id,
                "get_external_engagement",
                &owner,
            )?;
            reconcile_engagement(&mut store, &view, &state_root, engagement_id, &owner)?;
            let snapshot = store.external_snapshot(engagement_id)?;
            let mut value = serde_json::to_value(snapshot).map_err(internal)?;
            value["operation"] = json!("get_external_engagement_result");
            Ok(value)
        }
        Request::HireExternalSpecialist {
            engagement_id,
            role_ref,
            agent_configuration,
            objective,
            requested_access,
            idempotency_key,
        } => {
            authorize(
                &store,
                &view.workspace_id,
                engagement_id,
                "hire_external_specialist",
                &owner,
            )?;
            reject_nested_hire(&state_root)?;
            let child =
                carrier_from_options(arguments, "--new-controller-file", "--new-controller-fd")?;
            let prepared =
                prepare_external_specialist(Some(&workspace), &role_ref, agent_configuration)?;
            if prepared.view.workspace_id != view.workspace_id {
                return Err(integrity(
                    "prepared Specialist workspace differs from the engagement workspace",
                ));
            }
            if requested_access != "read_only"
                && prepared.agent_configuration.execution_lane.as_str() != "dedicated"
            {
                return Err(MachineError::new(
                    "SPECIALIST_POLICY_DENIED",
                    "write-capable Specialists require the dedicated execution lane",
                    false,
                    json!({}),
                ));
            }
            let reservation = store.reserve_external_hire(
                engagement_id,
                &role_ref,
                &prepared.agent_configuration,
                &objective,
                &requested_access,
                &idempotency_key,
            )?;
            if reservation.state != "provisioning" {
                return hire_result(&reservation);
            }
            let launch_cwd = match prepare_launch_root(
                &view.canonical_path.to_path_buf()?,
                view.mode,
                &state_root,
                engagement_id,
                reservation.specialist_run_id,
                &requested_access,
            ) {
                Ok(root) => root,
                Err(error) => {
                    store.finish_external_hire(
                        &reservation,
                        RuntimeOutcome::Rejected,
                        &idempotency_key,
                    )?;
                    let _ = cleanup_isolated_root(
                        &view.canonical_path.to_path_buf()?,
                        &state_root,
                        engagement_id,
                        reservation.specialist_run_id,
                    );
                    return Err(error);
                }
            };
            let binding = AggregateBinding {
                aggregate_kind: AggregateKind::ExternalSpecialistEngagement,
                aggregate_id: engagement_id,
                operation_id: reservation.hire_operation_id,
                member_kind: AggregateMemberKind::Specialist,
                policy_sha256: None,
                role_reference: Some(role_ref.clone()),
                role_snapshot_sha256: Some(crate::jcs::sha256_hex(role_ref.as_bytes())),
                agent_configuration_sha256: Some(
                    agent_configuration_digest(&prepared.agent_configuration).map_err(internal)?,
                ),
            };
            let args = specialist_start_arguments(
                &workspace,
                child.raw_fd(),
                &prepared.agent_configuration,
                engagement_id,
                reservation.hire_operation_id,
            );
            let sandbox = initial_specialist_sandbox(&requested_access);
            let outcome = start_external_specialist_run(
                &args,
                ExternalSpecialistStartContext {
                    reserved_run_id: reservation.specialist_run_id,
                    aggregate_binding: &binding,
                    agent_configuration: &prepared.agent_configuration,
                    launch_cwd: launch_cwd.as_deref(),
                    sandbox,
                },
            );
            let run_root = run_root(&state_root, reservation.specialist_run_id);
            let runtime_outcome = if outcome.is_ok() {
                RuntimeOutcome::Accepted
            } else if run_root.exists() {
                RuntimeOutcome::Unknown
            } else {
                RuntimeOutcome::Rejected
            };
            let final_reservation =
                store.finish_external_hire(&reservation, runtime_outcome, &idempotency_key)?;
            if runtime_outcome == RuntimeOutcome::Rejected {
                cleanup_isolated_root(
                    &view.canonical_path.to_path_buf()?,
                    &state_root,
                    engagement_id,
                    reservation.specialist_run_id,
                )?;
            }
            outcome?;
            hire_result(&final_reservation)
        }
        Request::AssignExternalSpecialistTask {
            engagement_id,
            specialist_run_id,
            external_request_ref,
            objective,
            context_refs,
            expected_output,
            execution_intent,
            deadline_seconds,
            idempotency_key,
        } => {
            authorize(
                &store,
                &view.workspace_id,
                engagement_id,
                "assign_external_specialist_task",
                &owner,
            )?;
            let external_request_ref =
                serde_json::to_value(external_request_ref).map_err(internal)?;
            let request_value = json!({
                "engagement_id": engagement_id, "specialist_run_id": specialist_run_id,
                "external_request_ref": external_request_ref, "objective": objective,
                "context_refs": context_refs, "expected_output": expected_output,
                "execution_intent": execution_intent, "deadline_seconds": deadline_seconds,
            });
            let reserved = store.reserve_external_task(
                engagement_id,
                specialist_run_id,
                ExternalTaskRequest {
                    request_value: &request_value,
                    objective: &objective,
                    external_request_ref: &external_request_ref,
                    execution_intent: &execution_intent,
                    deadline_seconds,
                    idempotency_key: &idempotency_key,
                },
            )?;
            if reserved.state != "accepted" {
                return assign_result(&reserved);
            }
            let canonical_write = execution_intent == "canonical_workspace_write";
            if canonical_write {
                if let Err(error) = acquire_external_writer(
                    &view,
                    &state_root,
                    specialist_run_id,
                    engagement_id,
                    &owner,
                )
                .map_err(map_writer_conflict)
                {
                    store.finish_external_task(
                        engagement_id,
                        reserved.task_id,
                        None,
                        "failed",
                        Some(&error.code),
                    )?;
                    return Err(error);
                }
            } else if let Err(error) =
                ensure_external_specialist_worker(&view, &state_root, specialist_run_id)
            {
                store.finish_external_task(
                    engagement_id,
                    reserved.task_id,
                    None,
                    "failed",
                    Some(&error.code),
                )?;
                return Err(error);
            }
            if let Err(error) =
                store.mark_external_member_resident(engagement_id, specialist_run_id)
            {
                store.finish_external_task(
                    engagement_id,
                    reserved.task_id,
                    None,
                    "failed",
                    Some(&error.code),
                )?;
                if canonical_write {
                    let _ = release_external_writer_safely(
                        &view,
                        &state_root,
                        specialist_run_id,
                        engagement_id,
                        &owner,
                    );
                }
                return Err(error);
            }
            let prompt = task_prompt(
                &objective,
                &context_refs,
                &expected_output,
                &execution_intent,
                deadline_seconds,
            );
            if let Err(error) = store.mark_external_task_dispatching(
                engagement_id,
                reserved.task_id,
                &idempotency_key,
            ) {
                store.finish_external_task(
                    engagement_id,
                    reserved.task_id,
                    None,
                    "failed",
                    Some(&error.code),
                )?;
                if canonical_write {
                    let _ = release_external_writer_safely(
                        &view,
                        &state_root,
                        specialist_run_id,
                        engagement_id,
                        &owner,
                    );
                }
                return Err(error);
            }
            let response = call_run_worker(
                &state_root,
                specialist_run_id,
                DarwinSystem.current_uid(),
                Some(owner.raw_fd()),
                |expected| ControlRequestV1::ExternalSubmit {
                    expected,
                    caller: None,
                    engagement_id,
                    request: TurnControlRequest {
                        message: prompt,
                        idempotency_key: idempotency_key.clone(),
                        effort: None,
                        images: Vec::new(),
                    },
                },
            );
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    require_dispatch_quiescence(
                        &view,
                        &state_root,
                        specialist_run_id,
                        engagement_id,
                        reserved.task_id,
                        &owner,
                    )?;
                    store.finish_external_task(
                        engagement_id,
                        reserved.task_id,
                        None,
                        "interrupted_unknown",
                        Some("INTERRUPTED_UNKNOWN"),
                    )?;
                    if execution_intent == "canonical_workspace_write" {
                        release_external_writer_safely(
                            &view,
                            &state_root,
                            specialist_run_id,
                            engagement_id,
                            &owner,
                        )?;
                    }
                    return Err(error.machine_error(specialist_run_id, &state_root));
                }
            };
            let turn_id = match response {
                ControlResponseV1::Accepted { accepted } => accepted.turn_id,
                ControlResponseV1::Terminal { terminal } => {
                    let output = terminal_output(
                        &store,
                        &state_root,
                        engagement_id,
                        reserved.task_id,
                        specialist_run_id,
                        &terminal.final_response,
                    )?;
                    let result = store.finish_external_task(
                        engagement_id,
                        reserved.task_id,
                        Some(&output),
                        "completed_not_delivered",
                        None,
                    )?;
                    if execution_intent == "canonical_workspace_write" {
                        release_external_writer_safely(
                            &view,
                            &state_root,
                            specialist_run_id,
                            engagement_id,
                            &owner,
                        )?;
                    }
                    return assign_result(&result);
                }
                ControlResponseV1::Failed {
                    code,
                    message,
                    retryable,
                    details,
                } => {
                    store.finish_external_task(
                        engagement_id,
                        reserved.task_id,
                        None,
                        "failed",
                        Some(&code),
                    )?;
                    if execution_intent == "canonical_workspace_write" {
                        release_external_writer_safely(
                            &view,
                            &state_root,
                            specialist_run_id,
                            engagement_id,
                            &owner,
                        )?;
                    }
                    return Err(MachineError::new(code, message, retryable, details));
                }
                _ => {
                    require_dispatch_quiescence(
                        &view,
                        &state_root,
                        specialist_run_id,
                        engagement_id,
                        reserved.task_id,
                        &owner,
                    )?;
                    store.finish_external_task(
                        engagement_id,
                        reserved.task_id,
                        None,
                        "interrupted_unknown",
                        Some("INTERRUPTED_UNKNOWN"),
                    )?;
                    if execution_intent == "canonical_workspace_write" {
                        release_external_writer_safely(
                            &view,
                            &state_root,
                            specialist_run_id,
                            engagement_id,
                            &owner,
                        )?;
                    }
                    return Err(integrity(
                        "Specialist submit returned an unexpected response",
                    ));
                }
            };
            let running = store.mark_external_task_running(
                engagement_id,
                reserved.task_id,
                &turn_id,
                &idempotency_key,
            )?;
            assign_result(&running)
        }
        Request::AwaitExternalSpecialistTasks {
            engagement_id,
            task_ids,
            return_when,
            transport_wait_seconds,
        } => {
            authorize(
                &store,
                &view.workspace_id,
                engagement_id,
                "await_external_specialist_tasks",
                &owner,
            )?;
            if !(1..=60).contains(&transport_wait_seconds)
                || !matches!(return_when.as_str(), "any" | "all")
                || task_ids.is_empty()
                || task_ids.len() > 32
            {
                return Err(MachineError::invalid_argument(
                    "await",
                    "invalid task wait request",
                ));
            }
            reconcile_engagement(&mut store, &view, &state_root, engagement_id, &owner)?;
            let wait_deadline = Instant::now() + Duration::from_secs(transport_wait_seconds);
            for task in store.external_tasks(engagement_id, &task_ids)? {
                if task.state != "running" {
                    continue;
                }
                let Some(turn_id) = task.turn_id.as_deref() else {
                    continue;
                };
                let remaining = wait_deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let response = call_run_worker(
                    &state_root,
                    task.specialist_run_id,
                    DarwinSystem.current_uid(),
                    None,
                    |expected| ControlRequestV1::Wait {
                        expected,
                        caller: None,
                        turn_id: turn_id.to_owned(),
                        timeout_ms: Some(
                            u64::try_from(remaining.as_millis())
                                .unwrap_or(u64::MAX)
                                .max(1),
                        ),
                    },
                );
                reconcile_wait(
                    &mut store,
                    &state_root,
                    engagement_id,
                    task.task_id,
                    response,
                )?;
                let updated = store.external_task(engagement_id, task.task_id)?;
                if terminal_state(&updated.state)
                    && store.external_task_execution_intent(engagement_id, task.task_id)?
                        == "canonical_workspace_write"
                {
                    release_external_writer_safely(
                        &view,
                        &state_root,
                        task.specialist_run_id,
                        engagement_id,
                        &owner,
                    )?;
                }
                if return_when == "any"
                    && terminal_state(&store.external_task(engagement_id, task.task_id)?.state)
                {
                    break;
                }
            }
            reconcile_engagement(&mut store, &view, &state_root, engagement_id, &owner)?;
            let tasks = store.external_tasks(engagement_id, &task_ids)?;
            let pending = tasks
                .iter()
                .filter(|task| !terminal_state(&task.state))
                .map(|task| task.task_id)
                .collect::<Vec<_>>();
            Ok(
                json!({"operation":"await_external_specialist_tasks_result","tasks":task_values(tasks),"pending":pending}),
            )
        }
        Request::CollectExternalSpecialistResults {
            engagement_id,
            after_sequence,
            limit,
        } => {
            authorize(
                &store,
                &view.workspace_id,
                engagement_id,
                "collect_external_specialist_results",
                &owner,
            )?;
            reconcile_engagement(&mut store, &view, &state_root, engagement_id, &owner)?;
            let (tasks, next) =
                store.collect_external_results(engagement_id, after_sequence, limit)?;
            Ok(
                json!({"operation":"collect_external_specialist_results_result","tasks":task_values(tasks),"next_after_sequence":next}),
            )
        }
        Request::CancelExternalSpecialistTask {
            engagement_id,
            task_id,
            reason,
            idempotency_key,
        } => {
            bounded(&reason, 1024, "reason")?;
            authorize(
                &store,
                &view.workspace_id,
                engagement_id,
                "cancel_external_specialist_task",
                &owner,
            )?;
            let task = store.external_task(engagement_id, task_id)?;
            let was_terminal = terminal_state(&task.state);
            let result = if terminal_state(&task.state) {
                store.cancel_external_task(
                    engagement_id,
                    task_id,
                    if task.state == "cancelled" {
                        "cancelled"
                    } else {
                        "interrupted_unknown"
                    },
                    &reason,
                    &idempotency_key,
                )?
            } else if task.state == "accepted" {
                store.cancel_external_task(
                    engagement_id,
                    task_id,
                    "cancelled",
                    &reason,
                    &idempotency_key,
                )?
            } else {
                ensure_external_specialist_worker(&view, &state_root, task.specialist_run_id)?;
                let response = call_run_worker(
                    &state_root,
                    task.specialist_run_id,
                    DarwinSystem.current_uid(),
                    Some(owner.raw_fd()),
                    |expected| ControlRequestV1::ExternalInterrupt {
                        expected,
                        caller: None,
                        engagement_id,
                    },
                );
                let terminal = match response {
                    Ok(ControlResponseV1::Interrupted { turn_id, .. }) => call_run_worker(
                        &state_root,
                        task.specialist_run_id,
                        DarwinSystem.current_uid(),
                        None,
                        |expected| ControlRequestV1::Wait {
                            expected,
                            caller: None,
                            turn_id,
                            timeout_ms: Some(5_000),
                        },
                    )
                    .is_ok_and(|value| matches!(value, ControlResponseV1::Terminal { .. })),
                    Ok(ControlResponseV1::Terminal { .. }) => true,
                    _ => false,
                };
                store.cancel_external_task(
                    engagement_id,
                    task_id,
                    if terminal {
                        "cancelled"
                    } else {
                        "interrupted_unknown"
                    },
                    &reason,
                    &idempotency_key,
                )?
            };
            if store.external_task_execution_intent(engagement_id, task_id)?
                == "canonical_workspace_write"
            {
                let release = release_external_writer_safely(
                    &view,
                    &state_root,
                    result.specialist_run_id,
                    engagement_id,
                    &owner,
                );
                if let Err(error) = release
                    && !(result.state == "interrupted_unknown"
                        && error.code == "SPECIALIST_WRITER_CONFLICT"
                        && error.details.get("cause_code").and_then(Value::as_str)
                            == Some("RUN_STATE_CONFLICT"))
                {
                    return Err(error);
                }
            }
            let state = if result.state == "cancelled" {
                "cancelled"
            } else if result.state == "interrupted_unknown" {
                "interrupted_unknown"
            } else if was_terminal {
                "already_terminal"
            } else {
                "interrupted_unknown"
            };
            Ok(
                json!({"operation":"cancel_external_specialist_task_result","task_id":task_id,"state":state}),
            )
        }
        Request::ReleaseExternalSpecialist {
            engagement_id,
            specialist_run_id,
            reason,
            idempotency_key,
        } => {
            bounded(&reason, 1024, "reason")?;
            authorize(
                &store,
                &view.workspace_id,
                engagement_id,
                "release_external_specialist",
                &owner,
            )?;
            let (_, requested_access, member_state) =
                store.external_member_configuration(engagement_id, specialist_run_id)?;
            let actor_residency =
                store.external_member_actor_residency(engagement_id, specialist_run_id)?;
            if member_state == "released" {
                let state = store.release_external_member(
                    engagement_id,
                    specialist_run_id,
                    &reason,
                    &idempotency_key,
                )?;
                cleanup_isolated_root(
                    &view.canonical_path.to_path_buf()?,
                    &state_root,
                    engagement_id,
                    specialist_run_id,
                )?;
                return Ok(
                    json!({"operation":"release_external_specialist_result","specialist_run_id":specialist_run_id,"state":state}),
                );
            }
            if member_state == "provisioning" {
                return Err(MachineError::new(
                    "ENGAGEMENT_STATE_CONFLICT",
                    "Specialist hire is still within its provisioning lease",
                    true,
                    json!({"specialist_run_id":specialist_run_id,"required_action":"retry_after_reconciliation"}),
                ));
            }
            if store
                .external_active_tasks(engagement_id)?
                .iter()
                .any(|task| task.specialist_run_id == specialist_run_id)
            {
                return Err(MachineError::new(
                    "ENGAGEMENT_STATE_CONFLICT",
                    "Specialist has an active task",
                    false,
                    json!({"specialist_run_id":specialist_run_id}),
                ));
            }
            if actor_residency != "unavailable" {
                if requested_access == "canonical_workspace_write" {
                    release_external_writer_safely(
                        &view,
                        &state_root,
                        specialist_run_id,
                        engagement_id,
                        &owner,
                    )?;
                } else {
                    ensure_external_specialist_worker(&view, &state_root, specialist_run_id)?;
                }
                let response = call_run_worker(
                    &state_root,
                    specialist_run_id,
                    DarwinSystem.current_uid(),
                    Some(owner.raw_fd()),
                    |expected| ControlRequestV1::ExternalClose {
                        expected,
                        caller: None,
                        engagement_id,
                        interrupt: false,
                    },
                );
                match response {
                    Ok(ControlResponseV1::Closed { .. }) => {}
                    Ok(ControlResponseV1::Failed {
                        code,
                        message,
                        retryable,
                        details,
                    }) => return Err(MachineError::new(code, message, retryable, details)),
                    Ok(_) => {
                        return Err(integrity(
                            "Specialist close returned an unexpected response",
                        ));
                    }
                    Err(error) => return Err(error.machine_error(specialist_run_id, &state_root)),
                }
            }
            let state = store.release_external_member(
                engagement_id,
                specialist_run_id,
                &reason,
                &idempotency_key,
            )?;
            cleanup_isolated_root(
                &view.canonical_path.to_path_buf()?,
                &state_root,
                engagement_id,
                specialist_run_id,
            )?;
            Ok(
                json!({"operation":"release_external_specialist_result","specialist_run_id":specialist_run_id,"state":state}),
            )
        }
        Request::CloseExternalEngagement {
            engagement_id,
            mode,
            reason,
            idempotency_key,
        } => {
            bounded(&reason, 1024, "reason")?;
            authorize(
                &store,
                &view.workspace_id,
                engagement_id,
                "close_external_engagement",
                &owner,
            )?;
            if mode == "abort" {
                let snapshot = store.external_snapshot(engagement_id)?;
                for member in snapshot
                    .specialists
                    .iter()
                    .filter(|member| member.membership_state != "released")
                {
                    if member.membership_state == "provisioning" {
                        return Err(MachineError::new(
                            "ENGAGEMENT_STATE_CONFLICT",
                            "Specialist hire is still within its provisioning lease",
                            true,
                            json!({"specialist_run_id":member.specialist_run_id,"required_action":"retry_after_reconciliation"}),
                        ));
                    }
                    let (_, access, _) = store
                        .external_member_configuration(engagement_id, member.specialist_run_id)?;
                    abort_member(
                        &view,
                        &state_root,
                        engagement_id,
                        member.specialist_run_id,
                        &access,
                        &member.actor_residency,
                        &owner,
                    )?;
                    for task in store
                        .external_active_tasks(engagement_id)?
                        .into_iter()
                        .filter(|task| task.specialist_run_id == member.specialist_run_id)
                    {
                        store.finish_external_task(
                            engagement_id,
                            task.task_id,
                            None,
                            "cancelled",
                            None,
                        )?;
                    }
                    let release_key = format!(
                        "abort:{}",
                        crate::jcs::sha256_hex(
                            format!("{idempotency_key}\0{}", member.specialist_run_id).as_bytes()
                        )
                    );
                    store.release_external_member(
                        engagement_id,
                        member.specialist_run_id,
                        &reason,
                        &release_key,
                    )?;
                    cleanup_isolated_root(
                        &view.canonical_path.to_path_buf()?,
                        &state_root,
                        engagement_id,
                        member.specialist_run_id,
                    )?;
                }
            }
            let state =
                store.close_external_engagement(engagement_id, &mode, &reason, &idempotency_key)?;
            for member in store.external_snapshot(engagement_id)?.specialists {
                cleanup_isolated_root(
                    &view.canonical_path.to_path_buf()?,
                    &state_root,
                    engagement_id,
                    member.specialist_run_id,
                )?;
            }
            Ok(
                json!({"operation":"close_external_engagement_result","engagement_id":engagement_id,"state":state}),
            )
        }
    }
}

#[derive(Debug)]
#[must_use = "the engagement operation serializer is held only while this value is alive"]
struct EngagementOperationLock {
    file: File,
}

impl EngagementOperationLock {
    fn acquire(state_root: &Path, engagement_id: Uuid) -> Result<Self, MachineError> {
        let root = state_root.join("orchestration").join("engagement-locks");
        match std::fs::create_dir(&root) {
            Ok(()) => std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(internal)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(internal(error)),
        }
        let uid = DarwinSystem.current_uid();
        verify_secure_directory(&root, uid)?;
        let path = root.join(format!("{engagement_id}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(internal)?;
        verify_secure_file(&path, uid)?;
        DarwinSystem.lock_exclusive(&file).map_err(internal)?;
        Ok(Self { file })
    }
}

impl Drop for EngagementOperationLock {
    fn drop(&mut self) {
        let _ = DarwinSystem.unlock(&self.file);
    }
}

fn read_request(arguments: &[OsString]) -> Result<Request, MachineError> {
    let raw = option(arguments, "--request-fd")?
        .ok_or_else(|| MachineError::invalid_argument("--request-fd", "descriptor is required"))?;
    let fd = raw
        .parse::<i32>()
        .ok()
        .filter(|value| *value >= 0)
        .ok_or_else(|| {
            MachineError::invalid_argument("--request-fd", "descriptor must be nonnegative")
        })?;
    let mut file = File::open(format!("/dev/fd/{fd}"))
        .map_err(|_| MachineError::invalid_argument("--request-fd", "descriptor is unreadable"))?;
    let metadata = file.metadata().map_err(|_| {
        MachineError::invalid_argument("--request-fd", "descriptor metadata is unreadable")
    })?;
    if file.is_terminal()
        || !metadata.file_type().is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > MAX_REQUEST_BYTES
    {
        return Err(MachineError::invalid_argument(
            "--request-fd",
            "request carrier must be a same-uid mode-0600 regular file within bounds",
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MachineError::invalid_argument("--request-fd", "request is unreadable"))?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(MachineError::invalid_argument(
            "--request-fd",
            "request exceeds the byte limit",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        MachineError::invalid_argument(
            "--request-fd",
            format!("request does not match facade v1: {error}"),
        )
    })
}

fn authorize(
    store: &EngagementStore,
    workspace_id: &str,
    engagement_id: Uuid,
    operation: &str,
    owner: &CredentialCarrier,
) -> Result<(), MachineError> {
    store
        .authorize_external_owner(workspace_id, engagement_id, operation, owner)
        .map(|_| ())
}

fn reject_nested_hire(state_root: &Path) -> Result<(), MachineError> {
    let Some(thread_id) = std::env::var_os("CODEX_THREAD_ID") else {
        return Ok(());
    };
    let Some(thread_id) = thread_id.to_str() else {
        return Ok(());
    };
    if RunStore::new(SystemWorkspacePlatform, state_root)
        .external_specialist_thread_registered(thread_id)?
    {
        return Err(MachineError::new(
            "SPECIALIST_POLICY_DENIED",
            "an External Specialist cannot hire a nested first-class Specialist",
            false,
            json!({}),
        ));
    }
    Ok(())
}

fn specialist_start_arguments(
    workspace: &Path,
    child_fd: i32,
    configuration: &crate::run::AgentConfigurationSnapshot,
    engagement_id: Uuid,
    operation_id: Uuid,
) -> Vec<OsString> {
    let mut args = vec![
        "--workspace".into(),
        workspace.as_os_str().to_owned(),
        "--profile".into(),
        configuration.runtime_profile.clone().into(),
        "--control-mode".into(),
        "managed-agent".into(),
        "--execution-lane".into(),
        configuration.execution_lane.as_str().into(),
        "--required-assurance".into(),
        configuration.required_assurance.as_str().into(),
        "--purpose".into(),
        configuration.purpose.kind.as_str().into(),
        "--parent-namespace".into(),
        "dolgorae.external-specialist-engagement.v1".into(),
        "--parent-kind".into(),
        "specialist".into(),
        "--parent-id".into(),
        engagement_id.to_string().into(),
        "--model".into(),
        configuration.model.clone().into(),
        "--effort".into(),
        configuration.default_effort.clone().into(),
        "--instructions".into(),
        configuration.normalized_instructions.clone().into(),
        "--controller-fd".into(),
        child_fd.to_string().into(),
        "--idempotency-key".into(),
        format!("external-hire:{operation_id}").into(),
    ];
    if let Some(label) = &configuration.purpose.external_label {
        args.extend(["--purpose-label".into(), label.clone().into()]);
    }
    for capability in &configuration.required_capabilities {
        args.extend(["--require-capability".into(), capability.clone().into()]);
    }
    args
}

fn initial_specialist_sandbox(requested_access: &str) -> &'static str {
    if requested_access == "isolated_write" {
        "workspace-write"
    } else {
        "read-only"
    }
}

fn prepare_launch_root(
    canonical: &Path,
    mode: WorkspaceMode,
    state_root: &Path,
    engagement_id: Uuid,
    run_id: Uuid,
    access: &str,
) -> Result<Option<PathBuf>, MachineError> {
    match access {
        "read_only" => Ok(None),
        "canonical_workspace_write" => Ok(None),
        "isolated_write" if mode == WorkspaceMode::Git => {
            let root = isolated_root(state_root, engagement_id, run_id);
            match std::fs::symlink_metadata(&root) {
                Ok(_) => {
                    verify_secure_directory(&root, DarwinSystem.current_uid())?;
                    if !is_registered_git_worktree(canonical, &root) {
                        return Err(MachineError::new(
                            "RECOVERY_REQUIRED",
                            "the isolated Specialist worktree registration is invalid",
                            false,
                            json!({"run_id":run_id,"required_action":"restore_isolated_worktree"}),
                        ));
                    }
                    return Ok(Some(root));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(internal(error)),
            }
            create_private_directory(&isolated_container(state_root))?;
            create_private_directory(&isolated_engagement_root(state_root, engagement_id))?;
            if add_detached_git_worktree(canonical, &root).is_err() {
                return Err(MachineError::new(
                    "SPECIALIST_POLICY_DENIED",
                    "isolated Git worktree creation failed",
                    false,
                    json!({}),
                ));
            }
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(internal)?;
            verify_secure_directory(&root, DarwinSystem.current_uid())?;
            Ok(Some(root))
        }
        "isolated_write" => Err(MachineError::new(
            "SPECIALIST_POLICY_DENIED",
            "isolated write requires a Git workspace",
            false,
            json!({}),
        )),
        _ => Err(MachineError::invalid_argument(
            "requested_access",
            "unsupported Specialist access",
        )),
    }
}

fn create_private_directory(path: &Path) -> Result<(), MachineError> {
    match std::fs::create_dir(path) {
        Ok(()) => std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(internal)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(internal(error)),
    }
    verify_secure_directory(path, DarwinSystem.current_uid())
}

fn cleanup_isolated_root(
    canonical: &Path,
    state_root: &Path,
    engagement_id: Uuid,
    run_id: Uuid,
) -> Result<(), MachineError> {
    let root = isolated_root(state_root, engagement_id, run_id);
    match std::fs::symlink_metadata(&root) {
        Ok(_) => {
            verify_secure_directory(&root, DarwinSystem.current_uid())?;
            if !is_registered_git_worktree(canonical, &root) {
                return Err(MachineError::new(
                    "RECOVERY_REQUIRED",
                    "the isolated Specialist worktree registration is invalid",
                    false,
                    json!({"run_id":run_id,"required_action":"restore_isolated_worktree"}),
                ));
            }
            remove_git_worktree(canonical, &root).map_err(internal)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(internal(error)),
    }
    Ok(())
}

fn isolated_root(state_root: &Path, engagement_id: Uuid, run_id: Uuid) -> PathBuf {
    isolated_specialist_root(state_root, engagement_id, run_id)
}

fn isolated_engagement_root(state_root: &Path, engagement_id: Uuid) -> PathBuf {
    isolated_container(state_root).join(engagement_id.to_string())
}

fn isolated_container(state_root: &Path) -> PathBuf {
    state_root.join("orchestration").join("isolated")
}

fn reconcile_engagement(
    store: &mut EngagementStore,
    view: &WorkspaceView,
    state_root: &Path,
    engagement_id: Uuid,
    owner: &CredentialCarrier,
) -> Result<(), MachineError> {
    for (reservation, idempotency_key) in
        store.stale_external_hires(engagement_id, Duration::from_secs(300))?
    {
        let run_root = run_root(state_root, reservation.specialist_run_id);
        let outcome = if run_root.exists() {
            RuntimeOutcome::Unknown
        } else {
            RuntimeOutcome::Rejected
        };
        store.finish_external_hire(&reservation, outcome, &idempotency_key)?;
        if outcome == RuntimeOutcome::Rejected {
            cleanup_isolated_root(
                &view.canonical_path.to_path_buf()?,
                state_root,
                engagement_id,
                reservation.specialist_run_id,
            )?;
        }
    }
    for task in store.external_active_tasks(engagement_id)? {
        let canonical_write = store.external_task_execution_intent(engagement_id, task.task_id)?
            == "canonical_workspace_write";
        if task.state == "accepted" {
            if !store.external_task_deadline_expired(engagement_id, task.task_id)? {
                continue;
            }
            store.finish_external_task(
                engagement_id,
                task.task_id,
                None,
                "expired",
                Some("OPERATION_TIMEOUT"),
            )?;
            if canonical_write {
                release_external_writer_safely(
                    view,
                    state_root,
                    task.specialist_run_id,
                    engagement_id,
                    owner,
                )?;
            }
            continue;
        }
        if task.state == "dispatching" {
            if !store.external_task_deadline_expired(engagement_id, task.task_id)? {
                continue;
            }
            if !dispatch_boundary_is_quiescent(
                view,
                state_root,
                task.specialist_run_id,
                engagement_id,
                owner,
            )? {
                return Err(MachineError::new(
                    "OUTCOME_UNKNOWN",
                    "Specialist dispatch could not be proved quiescent",
                    false,
                    json!({"task_id":task.task_id,"required_action":"retry_reconciliation"}),
                ));
            }
            store.finish_external_task(
                engagement_id,
                task.task_id,
                None,
                "interrupted_unknown",
                Some("INTERRUPTED_UNKNOWN"),
            )?;
            if canonical_write {
                release_external_writer_safely(
                    view,
                    state_root,
                    task.specialist_run_id,
                    engagement_id,
                    owner,
                )?;
            }
            continue;
        }
        if task.state != "running" {
            store.finish_external_task(
                engagement_id,
                task.task_id,
                None,
                "interrupted_unknown",
                Some("INTERRUPTED_UNKNOWN"),
            )?;
            if canonical_write {
                release_external_writer_safely(
                    view,
                    state_root,
                    task.specialist_run_id,
                    engagement_id,
                    owner,
                )?;
            }
            continue;
        }
        ensure_external_specialist_worker(view, state_root, task.specialist_run_id)?;
        let response = call_run_worker(
            state_root,
            task.specialist_run_id,
            DarwinSystem.current_uid(),
            None,
            |expected| ControlRequestV1::RunStatus {
                expected,
                caller: None,
            },
        );
        match response {
            Ok(ControlResponseV1::Status {
                last_terminal: Some(terminal),
                ..
            }) if task.turn_id.as_deref() == Some(terminal.turn_id.as_str()) => {
                if terminal.status == "completed" {
                    let output = terminal_output(
                        store,
                        state_root,
                        engagement_id,
                        task.task_id,
                        task.specialist_run_id,
                        &terminal.final_response,
                    )?;
                    store.finish_external_task(
                        engagement_id,
                        task.task_id,
                        Some(&output),
                        "completed_not_delivered",
                        None,
                    )?;
                } else {
                    store.finish_external_task(
                        engagement_id,
                        task.task_id,
                        None,
                        "failed",
                        Some(&terminal.status),
                    )?;
                }
            }
            Ok(ControlResponseV1::Status { lifecycle, .. })
                if matches!(lifecycle.as_str(), "running" | "waiting_interaction") =>
            {
                let unsupported_interaction = lifecycle == "waiting_interaction";
                if unsupported_interaction
                    || store.external_task_deadline_expired(engagement_id, task.task_id)?
                {
                    if !interrupt_external_turn(
                        state_root,
                        task.specialist_run_id,
                        engagement_id,
                        owner,
                    )? {
                        return Err(outcome_unknown(task.task_id));
                    }
                    store.finish_external_task(
                        engagement_id,
                        task.task_id,
                        None,
                        if unsupported_interaction {
                            "failed"
                        } else {
                            "expired"
                        },
                        Some(if unsupported_interaction {
                            "SPECIALIST_INTERACTION_UNSUPPORTED"
                        } else {
                            "OPERATION_TIMEOUT"
                        }),
                    )?;
                }
            }
            Ok(ControlResponseV1::Status { lifecycle, .. })
                if matches!(lifecycle.as_str(), "idle" | "paused") =>
            {
                store.finish_external_task(
                    engagement_id,
                    task.task_id,
                    None,
                    "interrupted_unknown",
                    Some("INTERRUPTED_UNKNOWN"),
                )?;
            }
            Ok(ControlResponseV1::Status { .. }) => return Err(outcome_unknown(task.task_id)),
            Ok(_) => {
                return Err(integrity(
                    "Specialist status returned an unexpected response",
                ));
            }
            Err(error) => {
                return Err(error.machine_error(task.specialist_run_id, state_root));
            }
        }
        let updated = store.external_task(engagement_id, task.task_id)?;
        if terminal_state(&updated.state) && canonical_write {
            release_external_writer_safely(
                view,
                state_root,
                task.specialist_run_id,
                engagement_id,
                owner,
            )?;
        }
    }
    reconcile_idle_canonical_writer(store, view, state_root, engagement_id, owner)?;
    Ok(())
}

fn dispatch_boundary_is_quiescent(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    engagement_id: Uuid,
    owner: &CredentialCarrier,
) -> Result<bool, MachineError> {
    ensure_external_specialist_worker(view, state_root, run_id)?;
    let status = call_run_worker(
        state_root,
        run_id,
        DarwinSystem.current_uid(),
        None,
        |expected| ControlRequestV1::RunStatus {
            expected,
            caller: None,
        },
    )
    .map_err(|error| error.machine_error(run_id, state_root))?;
    let lifecycle = match status {
        ControlResponseV1::Status { lifecycle, .. } => lifecycle,
        _ => {
            return Err(integrity(
                "Specialist status returned an unexpected response",
            ));
        }
    };
    if matches!(lifecycle.as_str(), "idle" | "paused") {
        return Ok(true);
    }
    if !matches!(lifecycle.as_str(), "running" | "waiting_interaction") {
        return Ok(false);
    }
    interrupt_external_turn(state_root, run_id, engagement_id, owner)
}

fn interrupt_external_turn(
    state_root: &Path,
    run_id: Uuid,
    engagement_id: Uuid,
    owner: &CredentialCarrier,
) -> Result<bool, MachineError> {
    let interrupted = call_run_worker(
        state_root,
        run_id,
        DarwinSystem.current_uid(),
        Some(owner.raw_fd()),
        |expected| ControlRequestV1::ExternalInterrupt {
            expected,
            caller: None,
            engagement_id,
        },
    )
    .map_err(|error| error.machine_error(run_id, state_root))?;
    match interrupted {
        ControlResponseV1::Terminal { .. } => Ok(true),
        ControlResponseV1::Interrupted { turn_id, .. } => call_run_worker(
            state_root,
            run_id,
            DarwinSystem.current_uid(),
            None,
            |expected| ControlRequestV1::Wait {
                expected,
                caller: None,
                turn_id,
                timeout_ms: Some(5_000),
            },
        )
        .map(|response| matches!(response, ControlResponseV1::Terminal { .. }))
        .map_err(|error| error.machine_error(run_id, state_root)),
        ControlResponseV1::Failed {
            code,
            message,
            retryable,
            details,
        } => Err(MachineError::new(code, message, retryable, details)),
        _ => Err(integrity(
            "Specialist interrupt returned an unexpected response",
        )),
    }
}

fn require_dispatch_quiescence(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    engagement_id: Uuid,
    task_id: Uuid,
    owner: &CredentialCarrier,
) -> Result<(), MachineError> {
    if dispatch_boundary_is_quiescent(view, state_root, run_id, engagement_id, owner)? {
        Ok(())
    } else {
        Err(outcome_unknown(task_id))
    }
}

fn release_external_writer_safely(
    view: &WorkspaceView,
    state_root: &Path,
    run_id: Uuid,
    engagement_id: Uuid,
    owner: &CredentialCarrier,
) -> Result<(), MachineError> {
    let writer =
        crate::writer::WriterStore::new(state_root, &view.workspace_id, DarwinSystem.current_uid())
            .load()?;
    if writer
        .holder
        .as_ref()
        .is_none_or(|holder| holder.run_id != run_id)
    {
        return Ok(());
    }
    let projection =
        RunStore::new(SystemWorkspacePlatform, state_root).load_state_projection(run_id)?;
    if external_worker_ensure_required(projection.lifecycle) {
        ensure_external_specialist_worker(view, state_root, run_id)?;
    }
    release_external_writer(view, state_root, run_id, engagement_id, owner)
        .map_err(map_writer_conflict)
}

const fn external_worker_ensure_required(lifecycle: RunLifecycle) -> bool {
    match lifecycle {
        RunLifecycle::Idle | RunLifecycle::Paused => true,
        RunLifecycle::Starting
        | RunLifecycle::Running
        | RunLifecycle::WaitingInteraction
        | RunLifecycle::ReconciliationRequired
        | RunLifecycle::Closed
        | RunLifecycle::StartFailed
        | RunLifecycle::OutcomeUnknown => false,
    }
}

fn reconcile_idle_canonical_writer(
    store: &EngagementStore,
    view: &WorkspaceView,
    state_root: &Path,
    engagement_id: Uuid,
    owner: &CredentialCarrier,
) -> Result<(), MachineError> {
    let eligible = store.external_canonical_members_without_active_tasks(engagement_id)?;
    let writer =
        crate::writer::WriterStore::new(state_root, &view.workspace_id, DarwinSystem.current_uid())
            .load()?;
    let Some(holder) = writer.holder else {
        return Ok(());
    };
    if eligible.contains(&holder.run_id) {
        release_external_writer_safely(view, state_root, holder.run_id, engagement_id, owner)?;
    }
    Ok(())
}

fn abort_member(
    view: &WorkspaceView,
    state_root: &Path,
    engagement_id: Uuid,
    run_id: Uuid,
    requested_access: &str,
    actor_residency: &str,
    owner: &CredentialCarrier,
) -> Result<(), MachineError> {
    let run_root = run_root(state_root, run_id);
    if actor_residency == "unavailable" && !run_root.exists() {
        return Ok(());
    }
    let projection =
        RunStore::new(SystemWorkspacePlatform, state_root).load_state_projection(run_id)?;
    if projection.lifecycle == RunLifecycle::Closed {
        return Ok(());
    }
    ensure_external_specialist_worker(view, state_root, run_id)?;
    let status = call_run_worker(
        state_root,
        run_id,
        DarwinSystem.current_uid(),
        None,
        |expected| ControlRequestV1::RunStatus {
            expected,
            caller: None,
        },
    )
    .map_err(|error| error.machine_error(run_id, state_root))?;
    let lifecycle = match status {
        ControlResponseV1::Status { lifecycle, .. } => lifecycle,
        _ => {
            return Err(integrity(
                "Specialist status returned an unexpected response",
            ));
        }
    };
    if matches!(lifecycle.as_str(), "running" | "waiting_interaction") {
        let response = call_run_worker(
            state_root,
            run_id,
            DarwinSystem.current_uid(),
            Some(owner.raw_fd()),
            |expected| ControlRequestV1::ExternalInterrupt {
                expected,
                caller: None,
                engagement_id,
            },
        )
        .map_err(|error| error.machine_error(run_id, state_root))?;
        let turn_id = match response {
            ControlResponseV1::Interrupted { turn_id, .. } => turn_id,
            ControlResponseV1::Terminal { terminal } => terminal.turn_id,
            ControlResponseV1::Failed {
                code,
                message,
                retryable,
                details,
            } => return Err(MachineError::new(code, message, retryable, details)),
            _ => {
                return Err(integrity(
                    "Specialist interrupt returned an unexpected response",
                ));
            }
        };
        let terminal = call_run_worker(
            state_root,
            run_id,
            DarwinSystem.current_uid(),
            None,
            |expected| ControlRequestV1::Wait {
                expected,
                caller: None,
                turn_id,
                timeout_ms: Some(5_000),
            },
        )
        .map_err(|error| error.machine_error(run_id, state_root))?;
        if !matches!(terminal, ControlResponseV1::Terminal { .. }) {
            return Err(MachineError::new(
                "OUTCOME_UNKNOWN",
                "Specialist did not reach a proved terminal state during abort",
                false,
                json!({"run_id":run_id}),
            ));
        }
    } else if !matches!(lifecycle.as_str(), "idle" | "paused") {
        return Err(MachineError::new(
            "RECOVERY_REQUIRED",
            "Specialist lifecycle is not safe to abort",
            false,
            json!({"run_id":run_id,"lifecycle":lifecycle}),
        ));
    }
    if requested_access == "canonical_workspace_write" {
        release_external_writer_safely(view, state_root, run_id, engagement_id, owner)?;
    }
    match call_run_worker(
        state_root,
        run_id,
        DarwinSystem.current_uid(),
        Some(owner.raw_fd()),
        |expected| ControlRequestV1::ExternalClose {
            expected,
            caller: None,
            engagement_id,
            interrupt: false,
        },
    ) {
        Ok(ControlResponseV1::Closed { .. }) => Ok(()),
        Ok(ControlResponseV1::Failed {
            code,
            message,
            retryable,
            details,
        }) => Err(MachineError::new(code, message, retryable, details)),
        Ok(_) => Err(integrity(
            "Specialist close returned an unexpected response",
        )),
        Err(error) => Err(error.machine_error(run_id, state_root)),
    }
}

fn reconcile_wait(
    store: &mut EngagementStore,
    state_root: &Path,
    engagement_id: Uuid,
    task_id: Uuid,
    response: Result<ControlResponseV1, crate::worker::WorkerProtocolError>,
) -> Result<(), MachineError> {
    match response {
        Ok(ControlResponseV1::Terminal { terminal }) if terminal.status == "completed" => {
            let task = store.external_task(engagement_id, task_id)?;
            let output = terminal_output(
                store,
                state_root,
                engagement_id,
                task_id,
                task.specialist_run_id,
                &terminal.final_response,
            )?;
            store.finish_external_task(
                engagement_id,
                task_id,
                Some(&output),
                "completed_not_delivered",
                None,
            )?;
        }
        Ok(ControlResponseV1::Terminal { terminal }) => {
            store.finish_external_task(
                engagement_id,
                task_id,
                None,
                "failed",
                Some(&terminal.status),
            )?;
        }
        Ok(ControlResponseV1::Running { .. } | ControlResponseV1::WaitingInteraction { .. }) => {}
        Ok(ControlResponseV1::Failed {
            code,
            message,
            retryable,
            details,
        }) => return Err(MachineError::new(code, message, retryable, details)),
        Ok(_) => return Err(integrity("Specialist wait returned an unexpected response")),
        Err(error) => {
            let task = store.external_task(engagement_id, task_id)?;
            return Err(error.machine_error(task.specialist_run_id, state_root));
        }
    }
    Ok(())
}

fn terminal_output(
    store: &EngagementStore,
    state_root: &Path,
    engagement_id: Uuid,
    task_id: Uuid,
    run_id: Uuid,
    response: &Option<FinalResponse>,
) -> Result<Value, MachineError> {
    let response = serde_json::to_value(response).map_err(internal)?;
    if store.external_task_execution_intent(engagement_id, task_id)? != "isolated_write" {
        return Ok(response);
    }
    let root = isolated_root(state_root, engagement_id, run_id);
    let patch_base64 = capture_isolated_change(&root, task_id)?;
    Ok(json!({
        "final_response": response,
        "isolated_change": {"format":"git_diff_binary_base64","patch_base64":patch_base64}
    }))
}

fn capture_isolated_change(root: &Path, task_id: Uuid) -> Result<String, MachineError> {
    let patch = capture_git_worktree_patch(root, 16 * 1024 * 1024).map_err(|_| {
        MachineError::new(
            "SPECIALIST_RESULT_INVALID",
            "isolated Specialist change artifact is unavailable or exceeds 16 MiB",
            false,
            json!({"task_id":task_id}),
        )
    })?;
    Ok(base64::engine::general_purpose::STANDARD.encode(patch))
}
fn terminal_state(state: &str) -> bool {
    matches!(
        state,
        "completed_not_delivered"
            | "delivered"
            | "failed"
            | "interrupted_unknown"
            | "cancelled"
            | "expired"
    )
}
fn task_values(tasks: Vec<ExternalTaskSnapshot>) -> Vec<Value> {
    tasks.into_iter().map(|task| json!({"task_id":task.task_id,"state":task.state,"result_artifact_ref":task.result_artifact_ref,"result":task.result,"safe_error_code":task.safe_error_code})).collect()
}
fn assign_result(task: &ExternalTaskSnapshot) -> Result<Value, MachineError> {
    Ok(
        json!({"operation":"assign_external_specialist_task_result","task_id":task.task_id,"state":task.state}),
    )
}
fn hire_result(hire: &crate::engagement::ExternalHireReservation) -> Result<Value, MachineError> {
    Ok(
        json!({"operation":"hire_external_specialist_result","engagement_id":hire.engagement_id,"hire_operation_id":hire.hire_operation_id,"specialist_run_id":hire.specialist_run_id,"state":hire.state}),
    )
}
fn task_prompt(
    objective: &str,
    refs: &[Uuid],
    expected: &[String],
    intent: &str,
    deadline_seconds: u64,
) -> String {
    format!(
        "Objective:\n{objective}\n\nContext artifact references: {}\nExpected output: {}\nExecution intent: {intent}\nTask deadline: {deadline_seconds} seconds after durable acceptance",
        refs.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        expected.join("; ")
    )
}
fn bounded(value: &str, maximum: usize, field: &str) -> Result<(), MachineError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        Err(MachineError::invalid_argument(
            field,
            "value must be nonempty and bounded",
        ))
    } else {
        Ok(())
    }
}
fn uuid7(value: Uuid, field: &str) -> Result<(), MachineError> {
    if value.get_version_num() == 7 {
        Ok(())
    } else {
        Err(MachineError::invalid_argument(
            field,
            "identifier must be UUIDv7",
        ))
    }
}
fn unique_uuid7(values: &[Uuid], maximum: usize, field: &str) -> Result<(), MachineError> {
    if values.len() > maximum {
        return Err(MachineError::invalid_argument(
            field,
            "too many identifiers",
        ));
    }
    let mut unique = std::collections::BTreeSet::new();
    for value in values {
        uuid7(*value, field)?;
        if !unique.insert(*value) {
            return Err(MachineError::invalid_argument(
                field,
                "identifiers must be unique",
            ));
        }
    }
    Ok(())
}
fn option(arguments: &[OsString], flag: &str) -> Result<Option<String>, MachineError> {
    crate::cli::option_path(arguments, flag)
        .map_err(|reason| MachineError::invalid_argument(flag, reason))
        .and_then(|value| {
            value
                .map(|value| {
                    value
                        .into_os_string()
                        .into_string()
                        .map_err(|_| MachineError::invalid_argument(flag, "value must be UTF-8"))
                })
                .transpose()
        })
}
fn integrity(message: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "external engagement integrity check failed",
        false,
        json!({"invariant":message}),
    )
}
fn outcome_unknown(task_id: Uuid) -> MachineError {
    MachineError::new(
        "OUTCOME_UNKNOWN",
        "Specialist execution could not be proved quiescent",
        true,
        json!({"task_id":task_id,"required_action":"retry_reconciliation"}),
    )
}
fn map_writer_conflict(error: MachineError) -> MachineError {
    MachineError::new(
        "SPECIALIST_WRITER_CONFLICT",
        "canonical workspace writer authority is unavailable",
        error.retryable,
        json!({"cause_code": error.code}),
    )
}
fn internal(error: impl ToString) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "external Specialist operation failed",
        false,
        json!({"reason":error.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn only_an_isolated_member_starts_with_a_write_sandbox() {
        assert_eq!(initial_specialist_sandbox("read_only"), "read-only");
        assert_eq!(
            initial_specialist_sandbox("canonical_workspace_write"),
            "read-only"
        );
        assert_eq!(
            initial_specialist_sandbox("isolated_write"),
            "workspace-write"
        );
    }

    #[test]
    fn engagement_operation_lock_serializes_one_engagement() {
        let state_root = std::env::temp_dir().join(format!(
            "dolgorae-external-operation-lock-{}",
            Uuid::now_v7()
        ));
        EngagementStore::open(&state_root.join("orchestration/orchestration.sqlite3")).unwrap();
        let engagement_id = Uuid::now_v7();
        let first = EngagementOperationLock::acquire(&state_root, engagement_id).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiting_root = state_root.clone();
        let joiner = std::thread::spawn(move || {
            let second = EngagementOperationLock::acquire(&waiting_root, engagement_id).unwrap();
            sender.send(()).unwrap();
            drop(second);
        });
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        joiner.join().unwrap();
        std::fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn isolated_launch_root_rejects_symlink_reuse() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-isolated-root-symlink-{}", Uuid::now_v7()));
        let state_root = root.join("state");
        let engagement_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let parent = state_root
            .join("orchestration/isolated")
            .join(engagement_id.to_string());
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(
            state_root.join("orchestration"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(
            state_root.join("orchestration/isolated"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(root.join("redirect")).unwrap();
        std::os::unix::fs::symlink(root.join("redirect"), parent.join(run_id.to_string())).unwrap();
        assert_eq!(
            prepare_launch_root(
                &root,
                WorkspaceMode::Git,
                &state_root,
                engagement_id,
                run_id,
                "isolated_write",
            )
            .unwrap_err()
            .code,
            "RUNTIME_PATH_INVALID"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn facade_request_validation_rejects_non_v7_and_duplicate_task_ids() {
        let invalid: Request = serde_json::from_value(json!({
            "operation":"get_external_engagement",
            "engagement_id":Uuid::nil(),
        }))
        .unwrap();
        assert_eq!(invalid.validate().unwrap_err().code, "INVALID_ARGUMENT");

        let task = Uuid::now_v7();
        let duplicate: Request = serde_json::from_value(json!({
            "operation":"await_external_specialist_tasks",
            "engagement_id":Uuid::now_v7(),
            "task_ids":[task,task],
            "return_when":"all",
            "transport_wait_seconds":1,
        }))
        .unwrap();
        assert_eq!(duplicate.validate().unwrap_err().code, "INVALID_ARGUMENT");
    }

    #[test]
    fn facade_request_validation_accepts_multiline_specialist_instructions() {
        let request: Request = serde_json::from_value(json!({
            "operation":"hire_external_specialist",
            "engagement_id":Uuid::now_v7(),
            "role_ref":"researcher",
            "agent_configuration":{
                "schema_version":1,
                "runtime_profile":"default",
                "model":null,
                "default_effort":"high",
                "purpose":"research",
                "purpose_label":null,
                "required_capabilities":[],
                "instructions":"First line.\nSecond line.",
                "execution_lane":"shared_readonly",
                "required_assurance":"best_effort_personal_alpha",
                "native_subagent_policy":"enabled"
            },
            "objective":"Inspect the target.\nReturn a concise result.",
            "requested_access":"read_only",
            "idempotency_key":"hire-1"
        }))
        .unwrap();
        request.validate().unwrap();
    }

    #[test]
    fn completed_task_projection_carries_the_durable_result() {
        let artifact_id = Uuid::now_v7();
        let values = task_values(vec![ExternalTaskSnapshot {
            task_id: Uuid::now_v7(),
            specialist_run_id: Uuid::now_v7(),
            state: "delivered".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            result_artifact_ref: Some(artifact_id),
            result: Some(json!({"kind":"inline","text":"done"})),
            safe_error_code: None,
        }]);
        assert_eq!(values[0]["result_artifact_ref"], artifact_id.to_string());
        assert_eq!(values[0]["result"]["text"], "done");
    }

    #[test]
    fn nullable_configuration_fields_must_still_be_present() {
        assert!(
            serde_json::from_value::<Request>(json!({
                "operation":"open_external_engagement",
                "external_controller_ref":{
                    "namespace":"test",
                    "kind":"workflow",
                    "id":"missing-label"
                },
                "idempotency_key":"open-required-nullable"
            }))
            .is_err()
        );
        let base = json!({
            "operation":"hire_external_specialist",
            "engagement_id":Uuid::now_v7(),
            "role_ref":"researcher",
            "agent_configuration":{
                "schema_version":1,
                "runtime_profile":"default",
                "model":null,
                "default_effort":"high",
                "purpose":"research",
                "purpose_label":null,
                "required_capabilities":[],
                "instructions":"Inspect.",
                "execution_lane":"shared_readonly",
                "required_assurance":"best_effort_personal_alpha",
                "native_subagent_policy":"enabled"
            },
            "objective":"Inspect.",
            "requested_access":"read_only",
            "idempotency_key":"hire-required-nullable"
        });
        for field in ["model", "purpose_label"] {
            let mut missing = base.clone();
            missing["agent_configuration"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
    }

    #[test]
    fn isolated_changes_are_captured_before_worktree_cleanup() {
        let root = std::env::temp_dir().join(format!("dolgorae-isolated-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let git = |arguments: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(arguments)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.name", "Dolgorae Test"]);
        git(&["config", "user.email", "test@example.invalid"]);
        std::fs::write(root.join("tracked.txt"), "before\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "--quiet", "-m", "baseline"]);
        std::fs::write(root.join("tracked.txt"), "after\n").unwrap();
        std::fs::write(root.join("untracked.txt"), "new\n").unwrap();

        let encoded = capture_isolated_change(&root, Uuid::now_v7()).unwrap();
        let patch = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
        )
        .unwrap();
        assert!(patch.contains("-before"));
        assert!(patch.contains("+after"));
        assert!(patch.contains("untracked.txt"));
        std::fs::write(root.join("latin1.txt"), [0xff, b'\n']).unwrap();
        let encoded = capture_isolated_change(&root, Uuid::now_v7()).unwrap();
        let patch = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(patch.contains(&0xff));
        assert!(capture_git_worktree_patch(&root, 64).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn closed_external_member_never_respawns_a_worker_before_writer_release() {
        for lifecycle in [RunLifecycle::Idle, RunLifecycle::Paused] {
            assert!(external_worker_ensure_required(lifecycle));
        }
        for lifecycle in [
            RunLifecycle::Starting,
            RunLifecycle::Running,
            RunLifecycle::WaitingInteraction,
            RunLifecycle::ReconciliationRequired,
            RunLifecycle::Closed,
            RunLifecycle::StartFailed,
            RunLifecycle::OutcomeUnknown,
        ] {
            assert!(!external_worker_ensure_required(lifecycle));
        }
    }
}
