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
    durable_terminal_turn, ensure_external_specialist_worker, finish_external_run_close,
    prepare_legacy_external_specialist, release_external_writer, start_external_specialist_run,
};
use crate::task_request::{STRUCTURED_REVIEW_OUTPUT, SpecialistTaskRequest};
use crate::turn::FinalResponse;
use crate::worker::{ControlRequestV1, ControlResponseV1, TurnControlRequest, call_run_worker};
use crate::workspace::{
    SourceIsolationContract, SystemWorkspacePlatform, WorkspaceService, WorkspaceView,
    WorktreePatchCaptureError, capture_git_worktree_patch, cleanup_isolated_root,
    initial_specialist_sandbox, isolated_specialist_root, prepare_legacy_launch_root,
    verify_secure_directory, verify_secure_file,
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
const FACADE_V3_SCHEMA: &str = "dolgorae-external-specialist-facade/v3";
const FACADE_V4_SCHEMA: &str = "dolgorae-external-specialist-facade/v4";

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HireConfiguration {
    Caller(crate::semantic::CallerAgentConfigurationInput),
    Legacy(ExternalAgentConfigurationInput),
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct OpaqueRef {
    namespace: String,
    kind: String,
    id: String,
}

#[derive(Debug, Default)]
enum OptionalField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

fn deserialize_optional_field<'de, D, T>(deserializer: D) -> Result<OptionalField<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(match Option::<T>::deserialize(deserializer)? {
        Some(value) => OptionalField::Value(value),
        None => OptionalField::Null,
    })
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
        #[serde(default, deserialize_with = "deserialize_optional_field")]
        schema: OptionalField<String>,
        agent_configuration: HireConfiguration,
        objective: String,
        requested_access: String,
        idempotency_key: String,
    },
    AssignExternalSpecialistTask {
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        external_request_ref: OpaqueRef,
        #[serde(default, deserialize_with = "deserialize_optional_field")]
        schema: OptionalField<String>,
        #[serde(default, deserialize_with = "deserialize_optional_field")]
        target: OptionalField<crate::review::ReviewTargetRequest>,
        #[serde(default, deserialize_with = "deserialize_optional_field")]
        objective: OptionalField<String>,
        #[serde(default, deserialize_with = "deserialize_optional_field")]
        task: OptionalField<SpecialistTaskRequest>,
        #[serde(default, deserialize_with = "deserialize_optional_field")]
        context_refs: OptionalField<Vec<Uuid>>,
        #[serde(default, deserialize_with = "deserialize_optional_field")]
        expected_output: OptionalField<Vec<String>>,
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
                schema,
                agent_configuration,
            } => {
                match (schema, agent_configuration) {
                    (OptionalField::Missing, HireConfiguration::Legacy(_)) => (),
                    (OptionalField::Value(schema), HireConfiguration::Caller(input))
                        if schema == FACADE_V4_SCHEMA && input.schema_version == 3 => {}
                    _ => {
                        return Err(MachineError::invalid_argument(
                            "agent_configuration",
                            "hire schema and configuration version do not match",
                        ));
                    }
                }
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
                schema,
                target,
                task,
                context_refs,
                expected_output,
                execution_intent,
                deadline_seconds,
                idempotency_key,
            } => {
                uuid7(*engagement_id, "engagement_id")?;
                uuid7(*specialist_run_id, "specialist_run_id")?;
                external_request_ref.validate("external_request_ref")?;
                match (schema, objective, task, context_refs, expected_output) {
                    (
                        OptionalField::Missing,
                        OptionalField::Value(objective),
                        OptionalField::Missing,
                        OptionalField::Value(context_refs),
                        OptionalField::Value(expected_output),
                    ) if matches!(target, OptionalField::Missing) => {
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
                    }
                    (
                        OptionalField::Value(schema),
                        OptionalField::Missing,
                        OptionalField::Value(task),
                        OptionalField::Missing,
                        OptionalField::Missing,
                    ) if schema == FACADE_V3_SCHEMA && matches!(target, OptionalField::Missing) => {
                        if task.expected_output == STRUCTURED_REVIEW_OUTPUT {
                            task.validate_review()?;
                        } else {
                            task.validate()?;
                        }
                    }
                    (
                        OptionalField::Value(schema),
                        OptionalField::Missing,
                        OptionalField::Value(task),
                        OptionalField::Missing,
                        OptionalField::Missing,
                    ) if schema == FACADE_V4_SCHEMA => {
                        if task.expected_output == "structured_review_v4" {
                            let OptionalField::Value(target) = target else {
                                return Err(MachineError::invalid_argument(
                                    "target",
                                    "v4 review requires a target",
                                ));
                            };
                            direct_task_request(task, target.clone(), *deadline_seconds)?.task()?;
                            if execution_intent != "read_only" {
                                return Err(MachineError::invalid_argument(
                                    "execution_intent",
                                    "direct review is read-only",
                                ));
                            }
                        } else {
                            task.validate()?;
                            if !matches!(target, OptionalField::Missing) {
                                return Err(MachineError::invalid_argument(
                                    "target",
                                    "target is available only for structured_review_v4",
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(MachineError::invalid_argument(
                            "task",
                            "use either the legacy objective/context_refs/expected_output shape or the checked facade v3 task shape",
                        ));
                    }
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
    execute_cli_with_source_contract(arguments, SourceIsolationContract::CallerWorkspace)
}

pub(crate) fn execute_cli_with_source_contract(
    arguments: &[OsString],
    source_contract: SourceIsolationContract,
) -> Result<Value, MachineError> {
    let request = read_request(arguments)?;
    request.validate()?;
    match &request {
        Request::HireExternalSpecialist {
            requested_access,
            agent_configuration,
            ..
        } => {
            source_contract.admit_access(requested_access)?;
            if source_contract == SourceIsolationContract::CallerWorkspace
                && matches!(agent_configuration, HireConfiguration::Legacy(_))
            {
                return Err(role_migration_required());
            }
        }
        Request::AssignExternalSpecialistTask {
            execution_intent, ..
        } => {
            source_contract.admit_access(execution_intent)?;
        }
        _ => {}
    }
    let workspace = crate::cli::option_path(arguments, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?
        .ok_or_else(|| MachineError::invalid_argument("--workspace", "workspace is required"))?;
    let view = WorkspaceService::system()?.discover(Some(&workspace))?;
    let state_root = DolgoraeHome::system()?.workspace_root(&view.workspace_id);
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
            schema: _,
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
            let agent_configuration = match agent_configuration {
                HireConfiguration::Caller(input) => {
                    return hire_caller_member(
                        &mut store,
                        &view,
                        &state_root,
                        engagement_id,
                        &role_ref,
                        input,
                        &objective,
                        &requested_access,
                        &idempotency_key,
                        &child,
                    );
                }
                HireConfiguration::Legacy(input) => input,
            };
            let prepared = prepare_legacy_external_specialist(
                Some(&workspace),
                &role_ref,
                agent_configuration,
            )?;
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
            let (reservation, first_attempt) = store.reserve_external_hire_with_provenance(
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
            let launch_cwd = match prepare_legacy_launch_root(
                &view.canonical_path.to_path_buf()?,
                view.mode,
                &state_root,
                engagement_id,
                reservation.specialist_run_id,
                &requested_access,
                first_attempt,
            ) {
                Ok(root) => root,
                Err(error) => {
                    store.finish_external_hire(
                        &reservation,
                        if first_attempt {
                            RuntimeOutcome::Rejected
                        } else {
                            RuntimeOutcome::Unknown
                        },
                        &idempotency_key,
                    )?;
                    if first_attempt {
                        let _ = cleanup_isolated_root(
                            &view.canonical_path.to_path_buf()?,
                            &state_root,
                            engagement_id,
                            reservation.specialist_run_id,
                        );
                    }
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
                    global_profile_binding: &prepared.global_profile_binding,
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
            schema,
            target,
            objective,
            task,
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
            let (mut request_value, objective, mut prompt) = match (
                schema,
                objective,
                task,
                context_refs,
                expected_output,
            ) {
                (
                    OptionalField::Value(schema),
                    OptionalField::Missing,
                    OptionalField::Value(task),
                    OptionalField::Missing,
                    OptionalField::Missing,
                ) => {
                    let prompt = format!(
                        "{}\n\nExecution intent: {execution_intent}\nTask deadline: {deadline_seconds} seconds after durable acceptance",
                        task.prompt()?
                    );
                    let objective = task.brief.clone();
                    (
                        json!({
                            "schema":schema,
                            "operation":"assign_external_specialist_task",
                            "engagement_id":engagement_id,
                            "specialist_run_id":specialist_run_id,
                            "external_request_ref":external_request_ref,
                            "task":task,
                            "execution_intent":execution_intent,
                            "deadline_seconds":deadline_seconds,
                        }),
                        objective,
                        prompt,
                    )
                }
                (
                    OptionalField::Missing,
                    OptionalField::Value(objective),
                    OptionalField::Missing,
                    OptionalField::Value(context_refs),
                    OptionalField::Value(expected_output),
                ) => {
                    let prompt = task_prompt(
                        &objective,
                        &context_refs,
                        &expected_output,
                        &execution_intent,
                        deadline_seconds,
                    );
                    (
                        json!({
                            "engagement_id": engagement_id, "specialist_run_id": specialist_run_id,
                            "external_request_ref": external_request_ref, "objective": objective,
                            "context_refs": context_refs, "expected_output": expected_output,
                            "execution_intent": execution_intent, "deadline_seconds": deadline_seconds,
                        }),
                        objective,
                        prompt,
                    )
                }
                _ => unreachable!("validated assignment shape is closed"),
            };
            let direct_request = match (&target, request_value.get("task")) {
                (OptionalField::Value(target), Some(task)) => Some(direct_task_request(
                    &serde_json::from_value(task.clone()).map_err(internal)?,
                    target.clone(),
                    deadline_seconds,
                )?),
                _ => None,
            };
            let manifest = RunStore::new(SystemWorkspacePlatform, &state_root)
                .load_manifest(specialist_run_id)?;
            if manifest.schema_version == 3
                && manifest.purpose.kind == crate::domain::PurposeKind::Review
                && direct_request.is_none()
                && !store.has_external_task_key(engagement_id, &idempotency_key)?
            {
                return Err(crate::direct_review_target::reading_boundary_unsupported());
            }
            if let Some(request) = &direct_request {
                request_value["target"] =
                    serde_json::to_value(&request.target).map_err(internal)?;
                if manifest.schema_version != 3
                    || manifest.purpose.kind != crate::domain::PurposeKind::Review
                {
                    return Err(crate::direct_review_target::reading_boundary_unsupported());
                }
            }
            let reserved = reserve_caller_external_task(
                &mut store,
                engagement_id,
                specialist_run_id,
                &manifest,
                direct_request.as_ref(),
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
            if let Some(request) = &direct_request {
                let prepared = crate::direct_review_target::DirectTargetReader::create_for_task(
                    &state_root,
                    &view.canonical_path.to_path_buf()?,
                    view.mode,
                    reserved.task_id,
                    specialist_run_id,
                    &request.target,
                )
                .and_then(|reader| crate::direct_review::review_prompt(request, reader.target()));
                match prepared {
                    Ok(value) => prompt = value,
                    Err(mut error) => {
                        store.finish_external_task(
                            engagement_id,
                            reserved.task_id,
                            None,
                            "failed",
                            Some(&error.code),
                        )?;
                        if error.details["schema"] == "dolgorae-workspace-error-details/v1" {
                            error.details["run_id"] = json!(specialist_run_id);
                        }
                        return Err(error);
                    }
                }
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
                        normalized_request_sha256: None,
                        write: false,
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
                    let result = match finish_assign_terminal(
                        &mut store,
                        &state_root,
                        engagement_id,
                        reserved.task_id,
                        specialist_run_id,
                        &idempotency_key,
                        &terminal,
                    ) {
                        Ok(result) => result,
                        Err(CompletionError::ResultConstructionPending(error)) => {
                            debug_assert!(error.retryable);
                            let task = store.external_task(engagement_id, reserved.task_id)?;
                            return assign_result(&task);
                        }
                        Err(CompletionError::Failure(error)) => {
                            let task = store.external_task(engagement_id, reserved.task_id)?;
                            if should_release_writer_after_completion_failure(
                                execution_intent == "canonical_workspace_write",
                                &task.state,
                            ) {
                                release_external_writer_safely(
                                    &view,
                                    &state_root,
                                    specialist_run_id,
                                    engagement_id,
                                    &owner,
                                )?;
                            }
                            return Err(error);
                        }
                    };
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
                let wait_outcome = reconcile_wait(
                    &mut store,
                    &state_root,
                    engagement_id,
                    task.task_id,
                    response,
                );
                match wait_outcome {
                    Ok(WaitReconciliation::Observed) => {}
                    Ok(WaitReconciliation::ResultConstructionPending) => continue,
                    Err(error) => {
                        let updated = store.external_task(engagement_id, task.task_id)?;
                        if should_release_writer(
                            store.external_task_execution_intent(engagement_id, task.task_id)?
                                == "canonical_workspace_write",
                            &updated.state,
                        ) {
                            release_external_writer_safely(
                                &view,
                                &state_root,
                                task.specialist_run_id,
                                engagement_id,
                                &owner,
                            )?;
                        }
                        return Err(error);
                    }
                }
                let updated = store.external_task(engagement_id, task.task_id)?;
                if should_release_writer(
                    store.external_task_execution_intent(engagement_id, task.task_id)?
                        == "canonical_workspace_write",
                    &updated.state,
                ) {
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
            let tasks = versioned_task_values(&store, engagement_id, tasks)?;
            let mut result = json!({"operation":"await_external_specialist_tasks_result","tasks":tasks,"pending":pending});
            mark_caller_result_page(&mut result);
            Ok(result)
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
            let tasks = versioned_task_values(&store, engagement_id, tasks)?;
            let mut result = json!({"operation":"collect_external_specialist_results_result","tasks":tasks,"next_after_sequence":next});
            mark_caller_result_page(&mut result);
            Ok(result)
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
                let terminal =
                    response.is_ok_and(|value| matches!(value, ControlResponseV1::Terminal { .. }));
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
                if run_root(&state_root, specialist_run_id).exists()
                    && RunStore::new(SystemWorkspacePlatform, &state_root)
                        .load_state_projection(specialist_run_id)?
                        .lifecycle
                        == RunLifecycle::Closed
                {
                    finish_external_run_close(
                        &view,
                        &state_root,
                        specialist_run_id,
                        engagement_id,
                        &owner,
                    )?;
                }
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
                let projection = RunStore::new(SystemWorkspacePlatform, &state_root)
                    .load_state_projection(specialist_run_id)?;
                if projection.lifecycle != RunLifecycle::Closed {
                    if requested_access == "canonical_workspace_write" {
                        release_external_writer_safely(
                            &view,
                            &state_root,
                            specialist_run_id,
                            engagement_id,
                            &owner,
                        )?;
                    }
                    ensure_external_specialist_worker(&view, &state_root, specialist_run_id)?;
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
                        Err(error) => {
                            return Err(error.machine_error(specialist_run_id, &state_root));
                        }
                    }
                }
                finish_external_run_close(
                    &view,
                    &state_root,
                    specialist_run_id,
                    engagement_id,
                    &owner,
                )?;
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

/// Keep original-key lookup and qualified admission adjacent to the task commit.
fn reserve_caller_external_task(
    store: &mut EngagementStore,
    engagement_id: Uuid,
    specialist_run_id: Uuid,
    manifest: &crate::run::RunManifest,
    direct_request: Option<&crate::direct_review::ReviewRequestV4>,
    request: ExternalTaskRequest<'_>,
) -> Result<ExternalTaskSnapshot, MachineError> {
    let original = store.has_external_task_key(engagement_id, request.idempotency_key)?;
    if !original {
        if manifest.schema_version >= 3
            && manifest
                .global_profile_binding
                .as_ref()
                .is_none_or(|binding| !binding.caller_workspace_supported())
        {
            return Err(crate::direct_review_target::reading_boundary_unsupported());
        }
        if let Some(direct) = direct_request {
            direct.admit_profile(
                manifest
                    .global_profile_binding
                    .as_ref()
                    .ok_or_else(crate::direct_review_target::reading_boundary_unsupported)?,
            )?;
        }
    }
    store.reserve_external_task(engagement_id, specialist_run_id, request)
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

fn isolated_root(state_root: &Path, engagement_id: Uuid, run_id: Uuid) -> PathBuf {
    isolated_specialist_root(state_root, engagement_id, run_id)
}

fn reconcile_engagement(
    store: &mut EngagementStore,
    view: &WorkspaceView,
    state_root: &Path,
    engagement_id: Uuid,
    owner: &CredentialCarrier,
) -> Result<(), MachineError> {
    reconcile_engagement_with(
        store,
        view,
        state_root,
        engagement_id,
        owner,
        |run_id| {
            ensure_external_specialist_worker(view, state_root, run_id)?;
            call_run_worker(
                state_root,
                run_id,
                DarwinSystem.current_uid(),
                None,
                |expected| ControlRequestV1::RunStatus {
                    expected,
                    caller: None,
                },
            )
            .map_err(|error| error.machine_error(run_id, state_root))
        },
        |run_id| durable_terminal_turn(state_root, run_id),
    )
}

fn reconcile_engagement_with<F, D>(
    store: &mut EngagementStore,
    view: &WorkspaceView,
    state_root: &Path,
    engagement_id: Uuid,
    owner: &CredentialCarrier,
    mut worker_status: F,
    mut durable_terminal: D,
) -> Result<(), MachineError>
where
    F: FnMut(Uuid) -> Result<ControlResponseV1, MachineError>,
    D: FnMut(Uuid) -> Result<Option<crate::turn::TerminalTurn>, MachineError>,
{
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
        if matches!(task.state.as_str(), "accepted" | "queued") {
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
            if canonical_write && task.state != "queued" {
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
        let response = worker_status(task.specialist_run_id);
        let response = match response {
            Ok(response) => Ok(recover_quiescent_terminal(response, || {
                durable_terminal(task.specialist_run_id)
            })?),
            Err(error) => Err(error),
        };
        match response {
            Ok(ControlResponseV1::Status {
                last_terminal: Some(terminal),
                ..
            }) if terminal_matches_task(&task, &terminal) => {
                if terminal.status == "completed" {
                    match finish_completed_task(
                        store,
                        state_root,
                        engagement_id,
                        task.task_id,
                        task.specialist_run_id,
                        &terminal.final_response,
                    ) {
                        Ok(_) => {}
                        Err(CompletionError::ResultConstructionPending(_)) => continue,
                        Err(CompletionError::Failure(error)) => {
                            let updated = store.external_task(engagement_id, task.task_id)?;
                            if should_release_writer(canonical_write, &updated.state) {
                                release_external_writer_safely(
                                    view,
                                    state_root,
                                    task.specialist_run_id,
                                    engagement_id,
                                    owner,
                                )?;
                            }
                            return Err(error);
                        }
                    }
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
            Err(error) => return Err(error),
        }
        let updated = store.external_task(engagement_id, task.task_id)?;
        if should_release_writer(canonical_write, &updated.state) {
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

fn recover_quiescent_terminal<F>(
    response: ControlResponseV1,
    durable_terminal: F,
) -> Result<ControlResponseV1, MachineError>
where
    F: FnOnce() -> Result<Option<crate::turn::TerminalTurn>, MachineError>,
{
    match response {
        ControlResponseV1::Status {
            identity,
            lifecycle,
            active_turn: None,
            last_terminal: None,
        } if matches!(lifecycle.as_str(), "idle" | "paused") => Ok(ControlResponseV1::Status {
            identity,
            lifecycle,
            active_turn: None,
            last_terminal: durable_terminal()?,
        }),
        response => Ok(response),
    }
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
        ControlResponseV1::Interrupted { .. } => Ok(false),
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
        return finish_external_run_close(view, state_root, run_id, engagement_id, owner);
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
        if !interrupt_external_turn(state_root, run_id, engagement_id, owner)? {
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
        Ok(ControlResponseV1::Closed { .. }) => {
            finish_external_run_close(view, state_root, run_id, engagement_id, owner)
        }
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

#[derive(Debug)]
enum CompletionError {
    ResultConstructionPending(MachineError),
    Failure(MachineError),
}

impl CompletionError {
    #[cfg(test)]
    fn machine_error(&self) -> &MachineError {
        match self {
            Self::ResultConstructionPending(error) | Self::Failure(error) => error,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitReconciliation {
    Observed,
    ResultConstructionPending,
}

fn reconcile_wait(
    store: &mut EngagementStore,
    state_root: &Path,
    engagement_id: Uuid,
    task_id: Uuid,
    response: Result<ControlResponseV1, crate::worker::WorkerProtocolError>,
) -> Result<WaitReconciliation, MachineError> {
    reconcile_wait_with(
        store,
        state_root,
        engagement_id,
        task_id,
        response,
        |store, engagement_id, task_id, run_id, response| {
            finish_completed_task(store, state_root, engagement_id, task_id, run_id, response)
        },
    )
}

fn reconcile_wait_with<F>(
    store: &mut EngagementStore,
    state_root: &Path,
    engagement_id: Uuid,
    task_id: Uuid,
    response: Result<ControlResponseV1, crate::worker::WorkerProtocolError>,
    mut finish_completed: F,
) -> Result<WaitReconciliation, MachineError>
where
    F: FnMut(
        &mut EngagementStore,
        Uuid,
        Uuid,
        Uuid,
        &Option<FinalResponse>,
    ) -> Result<ExternalTaskSnapshot, CompletionError>,
{
    match response {
        Ok(ControlResponseV1::Terminal { terminal }) if terminal.status == "completed" => {
            let task = store.external_task(engagement_id, task_id)?;
            match finish_completed(
                store,
                engagement_id,
                task_id,
                task.specialist_run_id,
                &terminal.final_response,
            ) {
                Ok(_) => {}
                Err(CompletionError::ResultConstructionPending(_)) => {
                    return Ok(WaitReconciliation::ResultConstructionPending);
                }
                Err(CompletionError::Failure(error)) => return Err(error),
            }
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
    Ok(WaitReconciliation::Observed)
}

fn finish_completed_task(
    store: &mut EngagementStore,
    state_root: &Path,
    engagement_id: Uuid,
    task_id: Uuid,
    run_id: Uuid,
    response: &Option<FinalResponse>,
) -> Result<ExternalTaskSnapshot, CompletionError> {
    let output = terminal_output(store, state_root, engagement_id, task_id, run_id, response);
    finish_completed_output(store, engagement_id, task_id, output)
}

fn finish_assign_terminal(
    store: &mut EngagementStore,
    state_root: &Path,
    engagement_id: Uuid,
    task_id: Uuid,
    run_id: Uuid,
    idempotency_key: &str,
    terminal: &crate::turn::TerminalTurn,
) -> Result<ExternalTaskSnapshot, CompletionError> {
    store
        .mark_external_task_running(engagement_id, task_id, &terminal.turn_id, idempotency_key)
        .map_err(CompletionError::Failure)?;
    finish_completed_task(
        store,
        state_root,
        engagement_id,
        task_id,
        run_id,
        &terminal.final_response,
    )
}

fn finish_completed_output(
    store: &mut EngagementStore,
    engagement_id: Uuid,
    task_id: Uuid,
    output: Result<Value, MachineError>,
) -> Result<ExternalTaskSnapshot, CompletionError> {
    let output = match output {
        Ok(output) => output,
        Err(error) if error.retryable => {
            if store
                .external_task_deadline_expired(engagement_id, task_id)
                .map_err(CompletionError::Failure)?
            {
                return store
                    .finish_external_task(
                        engagement_id,
                        task_id,
                        None,
                        "expired",
                        Some("OPERATION_TIMEOUT"),
                    )
                    .map_err(CompletionError::Failure);
            }
            store
                .mark_external_task_result_pending(engagement_id, task_id, &error.code)
                .map_err(CompletionError::Failure)?;
            return Err(CompletionError::ResultConstructionPending(error));
        }
        Err(mut error) => {
            let task = store
                .external_task(engagement_id, task_id)
                .map_err(CompletionError::Failure)?;
            crate::review_output::attach_execution(
                &mut error,
                crate::review_output::ReviewExecution {
                    run_id: Some(task.specialist_run_id),
                    task_id: Some(task_id),
                    turn_id: task.turn_id,
                    ..Default::default()
                },
            );
            let diagnostic = crate::review_output::diagnostic(&error);
            let stored = store
                .finish_external_task_with_diagnostic(
                    engagement_id,
                    task_id,
                    None,
                    "failed",
                    Some(&error.code),
                    diagnostic.as_ref(),
                )
                .map_err(CompletionError::Failure)?;
            if diagnostic.is_some() {
                let Some(stored_diagnostic) = stored.diagnostic.as_ref() else {
                    // A concurrent terminal transition won; expose its durable
                    // outcome rather than an invalid-output error we did not store.
                    return Ok(stored);
                };
                error.details["reason"] = json!(stored_diagnostic.category);
                error.details["diagnostic"] = json!(stored_diagnostic);
            }
            return Err(CompletionError::Failure(error));
        }
    };
    store
        .finish_external_task(
            engagement_id,
            task_id,
            Some(&output),
            "completed_not_delivered",
            None,
        )
        .map_err(CompletionError::Failure)
}

fn terminal_output(
    store: &EngagementStore,
    state_root: &Path,
    engagement_id: Uuid,
    task_id: Uuid,
    run_id: Uuid,
    response: &Option<FinalResponse>,
) -> Result<Value, MachineError> {
    let accepted_request = store.external_task_request(engagement_id, task_id)?;
    let structured = if accepted_request.get("schema").and_then(Value::as_str)
        == Some(FACADE_V4_SCHEMA)
        && accepted_request
            .pointer("/task/expected_output")
            .and_then(Value::as_str)
            == Some("structured_review_v4")
    {
        direct_task_output(
            store,
            state_root,
            engagement_id,
            task_id,
            run_id,
            &accepted_request,
            response,
        )
        .map(Some)
    } else {
        structured_task_output(&accepted_request, response)
    }
    .map_err(|mut error| {
        if crate::review_output::diagnostic(&error).is_some()
            && let Ok((configuration, _, _)) =
                store.external_member_configuration(engagement_id, run_id)
        {
            let codex_version = RunStore::new(SystemWorkspacePlatform, state_root)
                .load_manifest(run_id)
                .ok()
                .map(|manifest| manifest.profile.codex_version);
            crate::review_output::attach_execution(
                &mut error,
                crate::review_output::ReviewExecution {
                    model: Some(configuration.model),
                    effort: Some(configuration.default_effort),
                    codex_version,
                    run_id: Some(run_id),
                    task_id: Some(task_id),
                    turn_id: None,
                },
            );
        }
        error
    })?;
    let final_response = match structured {
        Some(output) => output,
        None => serde_json::to_value(response).map_err(internal)?,
    };
    if store.external_task_execution_intent(engagement_id, task_id)? != "isolated_write" {
        return Ok(final_response);
    }
    let root = isolated_root(state_root, engagement_id, run_id);
    let patch_base64 = capture_isolated_change(&root, task_id)?;
    Ok(json!({
        "final_response": final_response,
        "isolated_change": {"format":"git_diff_binary_base64","patch_base64":patch_base64}
    }))
}

fn direct_task_output(
    store: &EngagementStore,
    state_root: &Path,
    engagement_id: Uuid,
    task_id: Uuid,
    run_id: Uuid,
    accepted: &Value,
    response: &Option<FinalResponse>,
) -> Result<Value, MachineError> {
    let invalid = || crate::direct_review_target::provenance_invalid(task_id);
    let task: SpecialistTaskRequest =
        serde_json::from_value(accepted["task"].clone()).map_err(|_| invalid())?;
    let target = serde_json::from_value(accepted["target"].clone()).map_err(|_| invalid())?;
    let request = direct_task_request(
        &task,
        target,
        accepted["deadline_seconds"].as_u64().ok_or_else(invalid)?,
    )?;
    let reader =
        crate::direct_review_target::DirectTargetReader::open(state_root, task_id, run_id)?;
    if reader.target().kind != request.target.kind
        || reader.target().requested_revision != request.target.revision
    {
        return Err(invalid());
    }
    let terminal =
        crate::semantic::durable_terminal_turn(state_root, run_id)?.ok_or_else(invalid)?;
    let accepted_task = store.external_task(engagement_id, task_id)?;
    if accepted_task.specialist_run_id != run_id
        || !terminal_matches_task(&accepted_task, &terminal)
        || &terminal.final_response != response
    {
        return Err(invalid());
    }
    let projection =
        RunStore::new(SystemWorkspacePlatform, state_root).load_state_projection(run_id)?;
    let turn = json!({"run_id":run_id, "thread_id":terminal.thread_id,
        "turn_id":terminal.turn_id,"status":terminal.status});
    let verdict =
        crate::direct_review::completed_verdict(&turn, &terminal, &projection, &request, &reader)?;
    reader.check_acceptance()?;
    Ok(verdict)
}

fn structured_task_output(
    accepted_request: &Value,
    response: &Option<FinalResponse>,
) -> Result<Option<Value>, MachineError> {
    if accepted_request.get("schema").and_then(Value::as_str) == Some(FACADE_V3_SCHEMA) {
        let task: SpecialistTaskRequest = serde_json::from_value(
            accepted_request
                .get("task")
                .cloned()
                .ok_or_else(|| integrity("v3 task record lost its accepted task"))?,
        )
        .map_err(|_| integrity("v3 task record no longer matches its accepted shape"))?;
        if task.expected_output == "structured_review_v3" {
            task.validate_review()
                .map_err(|_| integrity("v3 task record no longer passes accepted validation"))?;
            let output = crate::specialist::parse_reviewer_response_v3(response.as_ref(), &task)?;
            return serde_json::to_value(output).map(Some).map_err(internal);
        }
    }
    Ok(None)
}

fn capture_isolated_change(root: &Path, task_id: Uuid) -> Result<String, MachineError> {
    let patch = capture_git_worktree_patch(root, 16 * 1024 * 1024)
        .map_err(|error| isolated_capture_error(task_id, error))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(patch))
}

fn isolated_capture_error(task_id: Uuid, error: WorktreePatchCaptureError) -> MachineError {
    match error {
        WorktreePatchCaptureError::Transient => outcome_unknown(task_id),
        WorktreePatchCaptureError::Unavailable | WorktreePatchCaptureError::LimitExceeded => {
            MachineError::new(
                "SPECIALIST_RESULT_INVALID",
                "isolated Specialist change artifact is unavailable or exceeds 16 MiB",
                false,
                json!({"task_id":task_id}),
            )
        }
    }
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

fn terminal_matches_task(
    task: &ExternalTaskSnapshot,
    terminal: &crate::turn::TerminalTurn,
) -> bool {
    task.turn_id.as_deref() == Some(terminal.turn_id.as_str())
}

fn should_release_writer(canonical_write: bool, task_state: &str) -> bool {
    canonical_write && terminal_state(task_state)
}

fn should_release_writer_after_completion_failure(canonical_write: bool, task_state: &str) -> bool {
    canonical_write && (terminal_state(task_state) || task_state == "dispatching")
}
fn task_values(tasks: Vec<ExternalTaskSnapshot>) -> Vec<Value> {
    tasks.into_iter().map(|task| {
        let mut value = json!({"task_id":task.task_id,"state":task.state,"result_artifact_ref":task.result_artifact_ref,"result":task.result,"safe_error_code":task.safe_error_code});
        if let Some(diagnostic) = task.diagnostic { value["diagnostic"] = json!(diagnostic); }
        value
    }).collect()
}

fn versioned_task_values(
    store: &EngagementStore,
    engagement_id: Uuid,
    tasks: Vec<ExternalTaskSnapshot>,
) -> Result<Vec<Value>, MachineError> {
    let mut values = task_values(tasks);
    for value in &mut values {
        let task_id = serde_json::from_value(value["task_id"].clone()).map_err(internal)?;
        let accepted = store.external_task_request(engagement_id, task_id)?;
        if accepted["schema"] == FACADE_V4_SCHEMA {
            value["task_contract"] = json!("dolgorae-task/v4");
            value["expected_output"] = accepted["task"]["expected_output"].clone();
        }
    }
    Ok(values)
}

fn mark_caller_result_page(result: &mut Value) {
    if result["tasks"].as_array().is_some_and(|tasks| {
        tasks
            .iter()
            .any(|task| task["task_contract"] == "dolgorae-task/v4")
    }) {
        result["schema"] = json!(FACADE_V4_SCHEMA);
    }
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

pub(crate) fn direct_task_request(
    task: &SpecialistTaskRequest,
    target: crate::review::ReviewTargetRequest,
    deadline_seconds: u64,
) -> Result<crate::direct_review::ReviewRequestV4, MachineError> {
    let request = crate::direct_review::ReviewRequestV4 {
        schema: "dolgorae-specialist-review-request/v4".to_owned(),
        operation: "review_target".to_owned(),
        target,
        purpose: task.purpose.clone(),
        brief: task.brief.clone(),
        contexts: task.contexts.clone(),
        criteria: task.criteria.clone(),
        expected_output: task.expected_output.clone(),
        deadline_seconds,
        role: None,
    };
    request.task()?;
    Ok(request)
}

fn role_migration_required() -> MachineError {
    MachineError::new(
        "ROLE_MIGRATION_REQUIRED",
        "new Specialists require an explicitly bound Markdown Role",
        false,
        json!({"schema":"dolgorae-workspace-error-details/v1", "required_action":"migrate_role",
            "phase":"admission","outcome":"not_dispatched","review_id":null,"run_id":null}),
    )
}

#[allow(clippy::too_many_arguments)]
fn hire_caller_member(
    store: &mut EngagementStore,
    view: &WorkspaceView,
    state_root: &Path,
    engagement_id: Uuid,
    role_ref: &str,
    input: crate::semantic::CallerAgentConfigurationInput,
    objective: &str,
    access: &str,
    key: &str,
    child: &CredentialCarrier,
) -> Result<Value, MachineError> {
    use crate::semantic::{
        CallerMemberStart, prepare_caller_agent_configuration_read_only, start_caller_member_run,
    };
    if input.purpose == "review" && access != "read_only" {
        return Err(MachineError::invalid_argument(
            "requested_access",
            "Reviewer sessions require read_only access",
        ));
    }
    let child_binding = binding_from_carrier(child, 1)?;
    if !matches!(
        child_binding.identity.kind,
        crate::domain::ControllerKind::Automation
            | crate::domain::ControllerKind::WorkflowOrchestrator
    ) {
        return Err(MachineError::invalid_argument(
            "new_controller",
            "Specialist requires an automation Controller",
        ));
    }
    let accepted = store.accepted_external_hire_configuration(engagement_id, key)?;
    let home = DolgoraeHome::system()?;
    let workspace = view.canonical_path.to_path_buf()?;
    let (configuration, prepared_binding) = match accepted {
        Some(configuration) => {
            require_matching_caller_hire(&input, &configuration)?;
            (configuration, None)
        }
        None => {
            let instructions =
                crate::instructions::InstructionSourceStore::new(&home).capture(&input.role)?;
            let prepared = prepare_caller_agent_configuration_read_only(
                Some(&workspace),
                Some(role_ref),
                input,
                instructions,
            )?;
            if prepared.agent_configuration.purpose.kind == crate::domain::PurposeKind::Review
                && !prepared.global_profile_binding.checked_review_supported()
            {
                return Err(crate::direct_review_target::reading_boundary_unsupported());
            }
            (
                prepared.agent_configuration.try_into()?,
                Some(prepared.global_profile_binding),
            )
        }
    };
    let (reservation, _) = store.reserve_external_hire_with_provenance(
        engagement_id,
        role_ref,
        &configuration,
        objective,
        access,
        key,
    )?;
    if reservation.state != "provisioning" {
        return hire_result(&reservation);
    }
    let instructions = configuration
        .instructions
        .caller()
        .ok_or_else(role_migration_required)?;
    let aggregate = AggregateBinding {
        aggregate_kind: AggregateKind::ExternalSpecialistEngagement,
        aggregate_id: engagement_id,
        operation_id: reservation.hire_operation_id,
        member_kind: AggregateMemberKind::Specialist,
        policy_sha256: None,
        role_reference: Some(role_ref.to_owned()),
        role_snapshot_sha256: Some(instructions.role.sha256.clone()),
        agent_configuration_sha256: Some(
            agent_configuration_digest(&configuration).map_err(internal)?,
        ),
    };
    let start_key = format!("caller-member-start:{}", reservation.hire_operation_id);
    let caller_access = if access == "canonical_workspace_write" {
        crate::domain::CallerWorkspaceAccess::Writable
    } else {
        crate::domain::CallerWorkspaceAccess::ReadOnly
    };
    let runs = RunStore::new(SystemWorkspacePlatform, state_root);
    let binding = resolve_caller_hire_binding(
        &home,
        view,
        state_root,
        &configuration,
        &aggregate,
        child,
        reservation.specialist_run_id,
        caller_access,
        &start_key,
        prepared_binding,
    )?;
    if configuration.purpose.kind == crate::domain::PurposeKind::Review
        && !binding.accepted_checked_review_supported()
    {
        return Err(crate::direct_review_target::reading_boundary_unsupported());
    }
    let outcome = start_caller_member_run(CallerMemberStart {
        view,
        state_root,
        configuration: &configuration,
        global_profile_binding: &binding,
        run_id: reservation.specialist_run_id,
        aggregate: &aggregate,
        carrier: child,
        access: caller_access,
        idempotency_key: start_key,
        reviewer_plan: None,
    });
    let runtime_outcome = if outcome.is_ok() {
        RuntimeOutcome::Accepted
    } else if runs.load_manifest(reservation.specialist_run_id).is_ok() {
        RuntimeOutcome::Unknown
    } else {
        RuntimeOutcome::Rejected
    };
    let reservation = store.finish_external_hire(&reservation, runtime_outcome, key)?;
    outcome?;
    hire_result(&reservation)
}

#[allow(clippy::too_many_arguments)]
fn resolve_caller_hire_binding(
    home: &DolgoraeHome,
    view: &WorkspaceView,
    state_root: &Path,
    configuration: &crate::run::AgentConfigurationSnapshot,
    aggregate: &AggregateBinding,
    child: &CredentialCarrier,
    run_id: Uuid,
    access: crate::domain::CallerWorkspaceAccess,
    start_key: &str,
    prepared: Option<crate::global_runtime::GlobalProfileBinding>,
) -> Result<crate::global_runtime::GlobalProfileBinding, MachineError> {
    let binding = match RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(run_id) {
        Ok(manifest) => manifest
            .global_profile_binding
            .ok_or_else(|| integrity("accepted member has no Profile binding"))?,
        Err(error) if error.code == "RUN_NOT_FOUND" => match prepared {
            Some(binding) => binding,
            None => match crate::semantic::retained_caller_member_binding(
                view,
                state_root,
                configuration,
                aggregate,
                child,
                run_id,
                access,
                start_key,
            )? {
                Some(binding) => binding,
                None => crate::global_runtime::ResolvedGlobalProfile::resolve(
                    home,
                    &configuration.runtime_profile,
                )?
                .prepare(home)?,
            },
        },
        Err(error) => return Err(error),
    };
    if binding.digest()? != configuration.runtime_profile_snapshot_sha256 {
        return Err(integrity(
            "original accepted Profile binding is unavailable",
        ));
    }
    Ok(binding)
}

fn require_matching_caller_hire(
    input: &crate::semantic::CallerAgentConfigurationInput,
    accepted: &crate::run::AgentConfigurationSnapshot,
) -> Result<(), MachineError> {
    let instructions = accepted
        .instructions
        .caller()
        .ok_or_else(role_migration_required)?;
    let mut requested_capabilities = input.required_capabilities.clone();
    requested_capabilities.sort();
    let mut accepted_capabilities = accepted.required_capabilities.clone();
    accepted_capabilities.sort();
    if input.schema_version != 3
        || accepted.schema_version != 3
        || input.selected_profile != accepted.runtime_profile
        || input.role != instructions.role.name
        || input
            .global_profile_binding_sha256
            .as_ref()
            .is_some_and(|digest| digest != &accepted.runtime_profile_snapshot_sha256)
        || input
            .model
            .as_ref()
            .is_some_and(|model| model != &accepted.model)
        || input.default_effort != accepted.default_effort
        || input.purpose != accepted.purpose.kind.as_str()
        || input.purpose_label != accepted.purpose.external_label
        || requested_capabilities != accepted_capabilities
        || input.execution_lane != accepted.execution_lane.as_str()
        || input.required_assurance != accepted.required_assurance.as_str()
        || input.native_subagent_policy != accepted.native_subagent_policy
    {
        return Err(MachineError::new(
            "IDEMPOTENCY_CONFLICT",
            "hire input differs from its original accepted configuration",
            false,
            json!({"required_action":"use_new_idempotency_key"}),
        ));
    }
    Ok(())
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
    use crate::domain::{
        Assurance, ControllerIdentity, ControllerKind, ExecutionLane, Purpose, PurposeKind,
    };
    use crate::run::{AgentConfigurationSnapshot, ControllerBinding, InstructionSnapshot};
    use crate::workspace::WorkspaceMode;
    use std::process::Command;

    #[test]
    fn historical_hire_recovers_the_sealed_start_binding_without_a_live_registry() {
        let fixture = crate::semantic::successor_fixture::SuccessorMember::historical();
        let legacy = fixture.legacy_member("retained-hire");
        let home = DolgoraeHome::from_canonical_home(fixture.root.clone()).unwrap();
        let manifest = &legacy.manifest;
        let configuration = &manifest.agent_configuration;
        let aggregate = manifest.aggregate_binding.as_ref().unwrap();
        let key = "caller-member-start:retained-hire";
        let reservation = crate::semantic::fixture_caller_member_reservation(
            &fixture.view,
            &fixture.state_root,
            manifest,
            &fixture.owner,
            key,
        );
        let before = serde_json::to_vec(&reservation).unwrap();
        std::fs::remove_dir_all(run_root(&fixture.state_root, legacy.run_id)).unwrap();
        assert!(!home.root().join("profiles.yaml").exists());
        let resolve = |cfg: &AgentConfigurationSnapshot, run, selected_key| {
            resolve_caller_hire_binding(
                &home,
                &fixture.view,
                &fixture.state_root,
                cfg,
                aggregate,
                &fixture.owner,
                run,
                crate::domain::CallerWorkspaceAccess::ReadOnly,
                selected_key,
                None,
            )
        };
        let restored = resolve(configuration, legacy.run_id, key).unwrap();
        assert_eq!(restored, reservation.accepted_intent.global_profile_binding);
        assert!(!restored.caller_workspace_supported());
        assert!(restored.accepted_caller_workspace_supported());
        let mut bad_digest = configuration.clone();
        bad_digest.runtime_profile_snapshot_sha256 = "0".repeat(64);
        assert!(resolve(&bad_digest, legacy.run_id, key).is_err());
        assert!(resolve(configuration, Uuid::now_v7(), key).is_err());
        let mut bad_role = configuration.clone();
        let original = configuration.instructions.caller().unwrap();
        let body = "A changed Role body.\n".to_owned();
        let role = crate::instructions::RoleCapture {
            name: original.role.name.clone(),
            byte_length: body.len() as u64,
            sha256: crate::jcs::sha256_hex(body.as_bytes()),
            body,
        };
        let instructions =
            crate::instructions::InstructionSnapshot::compose(original.common.clone(), role)
                .unwrap();
        bad_role.normalized_instructions = instructions.developer_prefix().unwrap();
        bad_role.instructions = instructions.into();
        assert!(resolve(&bad_role, legacy.run_id, key).is_err());
        assert!(resolve(configuration, legacy.run_id, "missing-start-key").is_err());
        let stored =
            crate::run::StartReservationStore::new(SystemWorkspacePlatform, &fixture.state_root)
                .load_caller(key, &reservation.request_sha256)
                .unwrap()
                .unwrap();
        assert_eq!(serde_json::to_vec(&stored).unwrap(), before);
        assert!(
            !run_root(&fixture.state_root, legacy.run_id)
                .join("manifest.json")
                .exists()
        );
    }

    #[test]
    fn historical_external_tasks_replay_but_every_new_output_refuses_before_commit() {
        let fixture = crate::semantic::successor_fixture::SuccessorMember::historical();
        let legacy = fixture.legacy_member("historical-cli");
        let manifest_before = serde_json::to_vec(&legacy.manifest).unwrap();
        let mut store = fixture.store();
        let external = json!({"namespace":"test","kind":"task","id":"historical"});
        let accepted = json!({"objective":"original accepted task"});
        fn request<'a>(
            value: &'a Value,
            key: &'a str,
            external: &'a Value,
        ) -> ExternalTaskRequest<'a> {
            ExternalTaskRequest {
                request_value: value,
                objective: "original accepted task",
                external_request_ref: external,
                execution_intent: "read_only",
                deadline_seconds: 60,
                idempotency_key: key,
            }
        }
        let original = store
            .reserve_external_task(
                fixture.engagement_id,
                legacy.run_id,
                request(&accepted, "accepted-old", &external),
            )
            .unwrap();
        let replay = reserve_caller_external_task(
            &mut store,
            fixture.engagement_id,
            legacy.run_id,
            &legacy.manifest,
            None,
            request(&accepted, "accepted-old", &external),
        )
        .unwrap();
        assert_eq!(replay, original);
        assert_eq!(
            reserve_caller_external_task(
                &mut store,
                fixture.engagement_id,
                legacy.run_id,
                &legacy.manifest,
                None,
                request(&json!({"objective":"changed"}), "accepted-old", &external)
            )
            .unwrap_err()
            .code,
            "IDEMPOTENCY_CONFLICT"
        );
        store
            .finish_external_task(
                fixture.engagement_id,
                original.task_id,
                None,
                "failed",
                Some("TURN_REJECTED"),
            )
            .unwrap();
        for output in ["plain_text", "structured_review_v3", "structured_review_v4"] {
            let before = store.external_snapshot(fixture.engagement_id).unwrap();
            let value = json!({"task":{"expected_output":output}});
            assert_eq!(
                reserve_caller_external_task(
                    &mut store,
                    fixture.engagement_id,
                    legacy.run_id,
                    &legacy.manifest,
                    None,
                    request(&value, output, &external)
                )
                .unwrap_err()
                .code,
                "REVIEW_READING_BOUNDARY_UNSUPPORTED"
            );
            assert_eq!(
                store.external_snapshot(fixture.engagement_id).unwrap(),
                before
            );
            assert!(
                !store
                    .has_external_task_key(fixture.engagement_id, output)
                    .unwrap()
            );
        }
        assert_eq!(
            serde_json::to_vec(&legacy.manifest).unwrap(),
            manifest_before
        );
    }

    #[test]
    fn caller_source_contract_refuses_external_isolation_before_carriers_or_workspace() {
        use std::os::fd::AsRawFd as _;
        let root =
            std::env::temp_dir().join(format!("dolgorae-retired-external-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let caller = root.join("caller");
        std::fs::create_dir(&caller).unwrap();
        std::fs::write(caller.join("keep.txt"), b"caller-owned").unwrap();
        let requests = [
            json!({
                "operation":"hire_external_specialist", "engagement_id":Uuid::now_v7(),
                "role_ref":"reviewer", "objective":"inspect", "requested_access":"isolated_write",
                "idempotency_key":"retired-hire", "agent_configuration":{
                    "schema_version":2, "selected_profile":"unused", "model":null,
                    "default_effort":"medium", "purpose":"review", "purpose_label":null,
                    "required_capabilities":[], "instructions":"inspect", "execution_lane":"dedicated",
                    "required_assurance":"best_effort_personal_alpha", "native_subagent_policy":"enabled"
                }
            }),
            json!({
                "operation":"assign_external_specialist_task", "engagement_id":Uuid::now_v7(),
                "specialist_run_id":Uuid::now_v7(), "external_request_ref":{
                    "namespace":"test", "kind":"task", "id":"retired-assignment"
                }, "objective":"inspect", "context_refs":[], "expected_output":["result"],
                "execution_intent":"isolated_write", "deadline_seconds":60, "idempotency_key":"retired-task"
            }),
        ];
        for (index, request) in requests.iter().enumerate() {
            let path = root.join(format!("request-{index}.json"));
            std::fs::write(&path, serde_json::to_vec(request).unwrap()).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let carrier = File::open(&path).unwrap();
            let args = [
                OsString::from("--request-fd"),
                OsString::from(carrier.as_raw_fd().to_string()),
                OsString::from("--workspace"),
                caller.as_os_str().to_owned(),
            ];
            let error =
                execute_cli_with_source_contract(&args, SourceIsolationContract::CallerWorkspace)
                    .unwrap_err();
            assert_eq!(error.code, "SOURCE_ISOLATION_RETIRED");
            assert_eq!(
                std::fs::read(caller.join("keep.txt")).unwrap(),
                b"caller-owned"
            );
            assert!(!caller.join(".dolgorae").exists());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn worker_identity(run_id: Uuid) -> crate::worker::WorkerIdentity {
        crate::worker::WorkerIdentity {
            workspace_id: "workspace".to_owned(),
            run_id,
            run_generation: 1,
            boot_uuid: Uuid::now_v7(),
            pid: std::process::id(),
            process_group_id: std::process::id(),
            session_id: std::process::id(),
            uid: DarwinSystem.current_uid(),
            start_tvsec: 1,
            start_tvusec: 0,
            executable_path: PathBuf::from("/usr/bin/true"),
            executable_device: 1,
            executable_inode: 1,
            executable_sha256: "a".repeat(64),
        }
    }

    #[test]
    fn failure_projections_preserve_contract_details_and_retryability() {
        let task_id = Uuid::now_v7();
        let error = outcome_unknown(task_id);
        assert_eq!(error.code, "OUTCOME_UNKNOWN");
        assert!(error.retryable);
        assert_eq!(
            error.details,
            json!({"task_id":task_id,"required_action":"retry_reconciliation"})
        );
        for (code, retryable) in [("WRITER_BUSY", true), ("RUN_STATE_CONFLICT", false)] {
            let error = map_writer_conflict(MachineError::new(
                code,
                "writer refusal",
                retryable,
                json!({"private_context":"must not be projected"}),
            ));
            assert_eq!(error.code, "SPECIALIST_WRITER_CONFLICT");
            assert_eq!(error.retryable, retryable);
            assert_eq!(error.details, json!({"cause_code":code}));
        }

        let root =
            std::env::temp_dir().join(format!("dolgorae-error-projections-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let error = capture_isolated_change(&root, task_id).unwrap_err();
        assert_eq!(error.code, "SPECIALIST_RESULT_INVALID");
        assert!(!error.retryable);
        assert_eq!(error.details, json!({"task_id":task_id}));

        let engagement_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let isolated = isolated_root(&root, engagement_id, run_id);
        std::fs::create_dir_all(&isolated).unwrap();
        std::fs::set_permissions(&isolated, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = prepare_legacy_launch_root(
            &root,
            WorkspaceMode::Git,
            &root,
            engagement_id,
            run_id,
            "isolated_write",
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, "RECOVERY_REQUIRED");
        assert!(!error.retryable);
        assert_eq!(
            error.details,
            json!({"run_id":run_id,"required_action":"restore_isolated_worktree"})
        );
        assert!(isolated.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

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
            prepare_legacy_launch_root(
                &root,
                WorkspaceMode::Git,
                &state_root,
                engagement_id,
                run_id,
                "isolated_write",
                true,
            )
            .unwrap_err()
            .code,
            "RUNTIME_PATH_INVALID"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn facade_request_validation_rejects_non_v7_bounds_and_duplicate_task_ids() {
        let invalid: Request = serde_json::from_value(json!({
            "operation":"get_external_engagement",
            "engagement_id":Uuid::nil(),
        }))
        .unwrap();
        assert_eq!(invalid.validate().unwrap_err().code, "INVALID_ARGUMENT");

        let unsupported_review_purpose: Request = serde_json::from_value(json!({
            "schema":"dolgorae-external-specialist-facade/v3",
            "operation":"assign_external_specialist_task",
            "engagement_id":Uuid::now_v7(),
            "specialist_run_id":Uuid::now_v7(),
            "external_request_ref":{"namespace":"test","kind":"review","id":"x"},
            "task":{
                "purpose":"analysis",
                "brief":"do not dispatch this malformed structured review",
                "contexts":[],
                "criteria":[],
                "expected_output":"structured_review_v3"
            },
            "execution_intent":"read_only",
            "deadline_seconds":600,
            "idempotency_key":"invalid-review-purpose-v3"
        }))
        .unwrap();
        assert_eq!(
            unsupported_review_purpose.validate().unwrap_err().code,
            "INVALID_ARGUMENT"
        );

        let v3_request = |task: Value, idempotency_key: &str| -> Request {
            serde_json::from_value(json!({
                "schema":"dolgorae-external-specialist-facade/v3",
                "operation":"assign_external_specialist_task",
                "engagement_id":Uuid::now_v7(),
                "specialist_run_id":Uuid::now_v7(),
                "external_request_ref":{"namespace":"test","kind":"review","id":"bounds"},
                "task":task,
                "execution_intent":"read_only",
                "deadline_seconds":600,
                "idempotency_key":idempotency_key,
            }))
            .unwrap()
        };
        let too_many_contexts = (0..65)
            .map(|index| {
                json!({
                    "id":format!("context-{index}"),
                    "content":"accepted context",
                    "provenance":"test",
                })
            })
            .collect::<Vec<_>>();
        let invalid = v3_request(
            json!({
                "purpose":"completion",
                "brief":"reject 65 contexts",
                "contexts":too_many_contexts,
                "criteria":[{"id":"C-1","statement":"bounded","source_context_ids":[]}],
                "expected_output":"structured_review_v3",
            }),
            "too-many-contexts",
        );
        assert_eq!(invalid.validate().unwrap_err().code, "INVALID_ARGUMENT");

        let too_many_criteria = (0..65)
            .map(|index| {
                json!({
                    "id":format!("criterion-{index}"),
                    "statement":"bounded criterion",
                    "source_context_ids":[],
                })
            })
            .collect::<Vec<_>>();
        let invalid = v3_request(
            json!({
                "purpose":"completion",
                "brief":"reject 65 criteria",
                "contexts":[],
                "criteria":too_many_criteria,
                "expected_output":"structured_review_v3",
            }),
            "too-many-criteria",
        );
        assert_eq!(invalid.validate().unwrap_err().code, "INVALID_ARGUMENT");

        let invalid = v3_request(
            json!({
                "purpose":"completion",
                "brief":"reject 65 context references",
                "contexts":[{"id":"context-0","content":"accepted context","provenance":"test"}],
                "criteria":[{
                    "id":"C-1",
                    "statement":"bounded references",
                    "source_context_ids":vec!["context-0"; 65],
                }],
                "expected_output":"structured_review_v3",
            }),
            "too-many-context-references",
        );
        assert_eq!(invalid.validate().unwrap_err().code, "INVALID_ARGUMENT");

        let mixed_null: Request = serde_json::from_value(json!({
            "schema":"dolgorae-external-specialist-facade/v3",
            "operation":"assign_external_specialist_task",
            "engagement_id":Uuid::now_v7(),
            "specialist_run_id":Uuid::now_v7(),
            "external_request_ref":{"namespace":"test","kind":"task","id":"x"},
            "objective":null,
            "task":{
                "purpose":"analysis",
                "brief":"inspect",
                "contexts":[],
                "criteria":[],
                "expected_output":"plain_text"
            },
            "execution_intent":"read_only",
            "deadline_seconds":600,
            "idempotency_key":"mixed-null-v3"
        }))
        .unwrap();
        assert_eq!(mixed_null.validate().unwrap_err().code, "INVALID_ARGUMENT");

        let incomplete_legacy: Request = serde_json::from_value(json!({
            "operation":"assign_external_specialist_task",
            "engagement_id":Uuid::now_v7(),
            "specialist_run_id":Uuid::now_v7(),
            "external_request_ref":{"namespace":"test","kind":"task","id":"x"},
            "objective":"inspect",
            "expected_output":["plain text"],
            "execution_intent":"read_only",
            "deadline_seconds":600,
            "idempotency_key":"missing-context-refs"
        }))
        .unwrap();
        assert_eq!(
            incomplete_legacy.validate().unwrap_err().code,
            "INVALID_ARGUMENT"
        );

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
                "schema_version":2,
                "selected_profile":"default",
                "global_profile_binding_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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
    fn facade_v3_accepts_separate_multiline_task_and_readable_context() {
        let request: Request = serde_json::from_value(json!({
            "schema":"dolgorae-external-specialist-facade/v3",
            "operation":"assign_external_specialist_task",
            "engagement_id":Uuid::now_v7(),
            "specialist_run_id":Uuid::now_v7(),
            "external_request_ref":{
                "namespace":"test",
                "kind":"review",
                "id":"TASK-039"
            },
            "task":{
                "purpose":"completion",
                "brief":"검토하세요.\n```sh\nprintf '$HOME'\n```",
                "contexts":[{
                    "id":"requirements",
                    "content":"C-1 source\r\nsecond line",
                    "provenance":"approved spec"
                }],
                "criteria":[{
                    "id":"C-1",
                    "statement":"Task content stays separate.",
                    "source_context_ids":["requirements"]
                }],
                "expected_output":"structured_review_v3"
            },
            "execution_intent":"read_only",
            "deadline_seconds":600,
            "idempotency_key":"assign-v3"
        }))
        .unwrap();
        request.validate().unwrap();

        let invalid: Request = serde_json::from_value(json!({
            "schema":"dolgorae-external-specialist-facade/v3",
            "operation":"assign_external_specialist_task",
            "engagement_id":Uuid::now_v7(),
            "specialist_run_id":Uuid::now_v7(),
            "external_request_ref":{"namespace":"test","kind":"review","id":"x"},
            "task":{
                "purpose":"completion",
                "brief":"missing criteria",
                "contexts":[],
                "criteria":[],
                "expected_output":"structured_review_v3"
            },
            "execution_intent":"read_only",
            "deadline_seconds":600,
            "idempotency_key":"invalid-v3"
        }))
        .unwrap();
        assert_eq!(invalid.validate().unwrap_err().code, "INVALID_ARGUMENT");
    }

    #[test]
    fn facade_v3_structured_result_is_checked_before_artifact_commit() {
        let accepted = json!({
            "schema":"dolgorae-external-specialist-facade/v3",
            "task":{
                "purpose":"completion",
                "brief":"Check completion.",
                "contexts":[],
                "criteria":[{
                    "id":"C-1",
                    "statement":"The requirement is met.",
                    "source_context_ids":[]
                }],
                "expected_output":"structured_review_v3"
            }
        });
        let report = json!({
            "summary":"reviewed",
            "findings":[],
            "criterion_assessments":[{
                "criterion_id":"C-1",
                "status":"met",
                "explanation":"candidate evidence is sufficient",
                "evidence":[{
                    "basis":"candidate",
                    "description":"checked source",
                    "path":"src/lib.rs",
                    "line_start":1,
                    "line_end":1,
                    "context_id":null
                }],
                "remaining_gap":null
            }],
            "evidence_limits":[],
            "overall_assessment":"requirements_met"
        });
        let response = Some(FinalResponse::Inline {
            text: serde_json::to_string(&report).unwrap(),
        });
        assert_eq!(
            structured_task_output(&accepted, &response).unwrap(),
            Some(report)
        );

        let missing = Some(FinalResponse::Inline {
            text: serde_json::to_string(&json!({
                "summary":"reviewed",
                "findings":[],
                "criterion_assessments":[],
                "evidence_limits":[],
                "overall_assessment":"requirements_met"
            }))
            .unwrap(),
        });
        assert_eq!(
            structured_task_output(&accepted, &missing)
                .unwrap_err()
                .code,
            "REVIEW_OUTPUT_INVALID"
        );
        let non_inline = structured_task_output(&accepted, &None).unwrap_err();
        assert_eq!(
            non_inline.details["diagnostic"]["category"],
            "output_unavailable"
        );
        assert!(non_inline.details["diagnostic"]["output_sha256"].is_null());
        let malformed = structured_task_output(
            &accepted,
            &Some(FinalResponse::Inline {
                text: "not json".to_owned(),
            }),
        )
        .unwrap_err();
        assert_eq!(malformed.details["diagnostic"]["category"], "invalid_json");
        assert_eq!(malformed.details["diagnostic"]["output_bytes"], 8);

        let oversized = "private-provider-output".repeat(50_000);
        let byte_length = oversized.len() as u64;
        let sha256 = crate::jcs::sha256_hex(oversized.as_bytes());
        for response in [
            FinalResponse::Artifact {
                artifact_id: Uuid::now_v7(),
                byte_length,
                sha256: sha256.clone(),
                created_at: "2026-09-27T00:00:00Z".to_owned(),
            },
            FinalResponse::Unavailable {
                byte_length,
                sha256: sha256.clone(),
                reason: "quota_exceeded".to_owned(),
            },
        ] {
            let error = structured_task_output(&accepted, &Some(response)).unwrap_err();
            let diagnostic = &error.details["diagnostic"];
            assert_eq!(diagnostic["category"], "output_too_large");
            assert_eq!(diagnostic["output_bytes"], byte_length);
            assert_eq!(diagnostic["output_sha256"], format!("sha256:{sha256}"));
            assert_eq!(diagnostic["known_top_level_keys"], json!([]));
            assert!(
                !serde_json::to_string(&error)
                    .unwrap()
                    .contains("private-provider")
            );
        }

        let mut corrupted = accepted.clone();
        corrupted["task"]["brief"] = json!("");
        let integrity_error = structured_task_output(&corrupted, &response).unwrap_err();
        assert_eq!(integrity_error.code, "INTERNAL_ERROR");
        assert_eq!(
            integrity_error.details["invariant"],
            "v3 task record no longer passes accepted validation"
        );
    }

    #[test]
    fn isolated_v3_result_preserves_report_and_retries_patch_capture() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-isolated-v3-result-{}", Uuid::now_v7()));
        let state_root = root.join("state");
        let mut store =
            EngagementStore::open(&state_root.join("orchestration/orchestration.sqlite3")).unwrap();
        let binding = ControllerBinding {
            identity: ControllerIdentity {
                controller_id: Uuid::now_v7(),
                kind: ControllerKind::Automation,
                instance_id: "test-host".to_owned(),
                subject_id: Some("test-principal".to_owned()),
                generation: 1,
            },
            capability_sha256: "a".repeat(64),
        };
        let opened = store
            .open_external_engagement(
                "workspace",
                &binding,
                &json!({"namespace":"test","kind":"workflow","id":"isolated-v3"}),
                None,
                "open-isolated-v3",
            )
            .unwrap();
        let configuration = AgentConfigurationSnapshot {
            schema_version: 2,
            runtime_profile: "test".to_owned(),
            runtime_profile_snapshot_sha256: "b".repeat(64),
            model: "test-model".to_owned(),
            default_effort: "medium".to_owned(),
            purpose: Purpose {
                kind: PurposeKind::Implementation,
                external_label: None,
            },
            required_capabilities: vec![],
            role_reference: Some("implementer".to_owned()),
            normalized_instructions: "Implement the task.".to_owned(),
            admitted_configuration: None,
            instructions: InstructionSnapshot {
                schema: "dolgorae-instruction-snapshot/v1".to_owned(),
                common_prefix_version: 1,
                mode_prefix_version: 1,
                purpose_prefix_version: 1,
                normalized_byte_length: 19,
                normalized_sha256: "c".repeat(64),
            }
            .into(),
            execution_lane: ExecutionLane::Dedicated,
            required_assurance: Assurance::BestEffortPersonalAlpha,
            native_subagent_policy: "enabled".to_owned(),
        };
        let hire = store
            .reserve_external_hire(
                opened.engagement_id,
                "implementer",
                &configuration,
                "Produce an isolated review change.",
                "isolated_write",
                "hire-isolated-v3",
            )
            .unwrap();
        let member = store
            .finish_external_hire(&hire, RuntimeOutcome::Accepted, "hire-isolated-v3")
            .unwrap();
        let task = SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "Check completion.".to_owned(),
            contexts: vec![],
            criteria: vec![crate::task_request::TaskCriterion {
                id: "C-1".to_owned(),
                statement: "The requirement is met.".to_owned(),
                source_context_ids: vec![],
            }],
            expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
        };
        let accepted = json!({
            "schema":FACADE_V3_SCHEMA,
            "operation":"assign_external_specialist_task",
            "task":&task
        });
        let external_ref = json!({"namespace":"test","kind":"review","id":"C-1"});
        let reserved = store
            .reserve_external_task(
                opened.engagement_id,
                member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &accepted,
                    objective: "Check completion.",
                    external_request_ref: &external_ref,
                    execution_intent: "isolated_write",
                    deadline_seconds: 60,
                    idempotency_key: "task-isolated-v3",
                },
            )
            .unwrap();
        store
            .mark_external_task_dispatching(
                opened.engagement_id,
                reserved.task_id,
                "task-isolated-v3",
            )
            .unwrap();
        store
            .mark_external_task_running(
                opened.engagement_id,
                reserved.task_id,
                "turn-isolated-v3",
                "task-isolated-v3",
            )
            .unwrap();
        let report = json!({
            "summary":"reviewed",
            "findings":[],
            "criterion_assessments":[{
                "criterion_id":"C-1",
                "status":"met",
                "explanation":"candidate evidence is sufficient",
                "evidence":[{"basis":"candidate","description":"checked source","path":"src/lib.rs","line_start":1,"line_end":1,"context_id":null}],
                "remaining_gap":null
            }],
            "evidence_limits":[],
            "overall_assessment":"requirements_met"
        });
        let response = Some(FinalResponse::Inline {
            text: serde_json::to_string(&report).unwrap(),
        });
        let terminal_response = || {
            Ok(ControlResponseV1::Terminal {
                terminal: serde_json::from_value(json!({
                    "thread_id":"thread-isolated-v3",
                    "turn_id":"turn-isolated-v3",
                    "status":"completed",
                    "effort":"medium",
                    "final_response":&response,
                    "usage":{"inputTokens":1,"outputTokens":1}
                }))
                .unwrap(),
            })
        };
        let pending = reconcile_wait_with(
            &mut store,
            &state_root,
            opened.engagement_id,
            reserved.task_id,
            terminal_response(),
            |store, engagement_id, task_id, run_id, observed_response| {
                assert_eq!(run_id, member.specialist_run_id);
                assert_eq!(observed_response, &response);
                finish_completed_output(
                    store,
                    engagement_id,
                    task_id,
                    Err(outcome_unknown(task_id)),
                )
            },
        )
        .unwrap();
        assert_eq!(pending, WaitReconciliation::ResultConstructionPending);
        let pending_task = store
            .external_task(opened.engagement_id, reserved.task_id)
            .unwrap();
        assert_eq!(pending_task.state, "running");
        assert_eq!(pending_task.result_artifact_ref, None);
        assert_eq!(pending_task.result, None);
        assert_eq!(
            pending_task.safe_error_code.as_deref(),
            Some("OUTCOME_UNKNOWN")
        );

        let other_hire = store
            .reserve_external_hire(
                opened.engagement_id,
                "reviewer",
                &configuration,
                "Observe while another result remains retryable.",
                "read_only",
                "hire-other-member",
            )
            .unwrap();
        let other_member = store
            .finish_external_hire(&other_hire, RuntimeOutcome::Accepted, "hire-other-member")
            .unwrap();
        let other_ref = json!({"namespace":"test","kind":"review","id":"other"});
        let other_task = store
            .reserve_external_task(
                opened.engagement_id,
                other_member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &json!({"task":"other"}),
                    objective: "Complete independently.",
                    external_request_ref: &other_ref,
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "task-other-member",
                },
            )
            .unwrap();
        store
            .mark_external_task_dispatching(
                opened.engagement_id,
                other_task.task_id,
                "task-other-member",
            )
            .unwrap();
        let other_terminal = serde_json::from_value(json!({
            "thread_id":"thread-other-member",
            "turn_id":"turn-other-member",
            "status":"completed",
            "effort":"medium",
            "final_response":{"kind":"inline","text":"other task completed"},
            "usage":{"inputTokens":1,"outputTokens":1}
        }))
        .unwrap();
        let other_finished = finish_assign_terminal(
            &mut store,
            &state_root,
            opened.engagement_id,
            other_task.task_id,
            other_member.specialist_run_id,
            "task-other-member",
            &other_terminal,
        )
        .unwrap();
        assert_eq!(other_finished.state, "completed_not_delivered");
        assert_eq!(
            store
                .external_task(opened.engagement_id, other_task.task_id)
                .unwrap()
                .turn_id
                .as_deref(),
            Some("turn-other-member")
        );
        assert_eq!(
            store
                .external_task(opened.engagement_id, reserved.task_id)
                .unwrap()
                .state,
            "running"
        );

        let isolated = isolated_root(&state_root, opened.engagement_id, member.specialist_run_id);
        std::fs::create_dir_all(&isolated).unwrap();
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&isolated)
            .output()
            .unwrap();
        std::fs::write(isolated.join("change.txt"), "baseline\n").unwrap();
        Command::new("git")
            .args(["add", "change.txt"])
            .current_dir(&isolated)
            .output()
            .unwrap();
        let committed = Command::new("git")
            .args([
                "-c",
                "user.name=Dolgorae Test",
                "-c",
                "user.email=dolgorae@example.invalid",
                "commit",
                "-m",
                "baseline",
            ])
            .current_dir(&isolated)
            .output()
            .unwrap();
        assert!(committed.status.success());
        std::fs::write(isolated.join("change.txt"), "isolated change\n").unwrap();
        assert_eq!(
            reconcile_wait(
                &mut store,
                &state_root,
                opened.engagement_id,
                reserved.task_id,
                terminal_response(),
            )
            .unwrap(),
            WaitReconciliation::Observed
        );
        let finished = store
            .external_task(opened.engagement_id, reserved.task_id)
            .unwrap();
        assert_eq!(finished.state, "completed_not_delivered");
        let result = finished.result.unwrap();
        assert_eq!(result["final_response"], report);
        assert_eq!(
            result["isolated_change"]["format"],
            "git_diff_binary_base64"
        );
        assert!(
            result["isolated_change"]["patch_base64"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        let expiring_ref = json!({"namespace":"test","kind":"review","id":"expiring"});
        let expiring = store
            .reserve_external_task(
                opened.engagement_id,
                member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &accepted,
                    objective: "Check completion.",
                    external_request_ref: &expiring_ref,
                    execution_intent: "isolated_write",
                    deadline_seconds: 1,
                    idempotency_key: "expiring-isolated-v3",
                },
            )
            .unwrap();
        store
            .mark_external_task_dispatching(
                opened.engagement_id,
                expiring.task_id,
                "expiring-isolated-v3",
            )
            .unwrap();
        store
            .mark_external_task_running(
                opened.engagement_id,
                expiring.task_id,
                "turn-expiring-isolated-v3",
                "expiring-isolated-v3",
            )
            .unwrap();
        store
            .expire_external_task_for_test(opened.engagement_id, expiring.task_id)
            .unwrap();
        assert_eq!(
            reconcile_wait_with(
                &mut store,
                &state_root,
                opened.engagement_id,
                expiring.task_id,
                terminal_response(),
                |store, engagement_id, task_id, _, _| {
                    finish_completed_output(
                        store,
                        engagement_id,
                        task_id,
                        Err(outcome_unknown(task_id)),
                    )
                },
            )
            .unwrap(),
            WaitReconciliation::Observed
        );
        let expired = store
            .external_task(opened.engagement_id, expiring.task_id)
            .unwrap();
        assert_eq!(expired.state, "expired");
        assert_eq!(
            expired.safe_error_code.as_deref(),
            Some("OPERATION_TIMEOUT")
        );
        assert_eq!(expired.result_artifact_ref, None);
        assert_eq!(expired.result, None);
        assert_eq!(
            finish_completed_output(
                &mut store,
                opened.engagement_id,
                expiring.task_id,
                Err(crate::review_output::invalid_output(
                    Some("late-invalid"),
                    None,
                    "invalid_json",
                    ""
                )),
            )
            .unwrap(),
            expired
        );
        assert_eq!(
            finish_completed_output(
                &mut store,
                opened.engagement_id,
                expiring.task_id,
                Err(outcome_unknown(expiring.task_id)),
            )
            .unwrap(),
            expired
        );
        let invalid_ref = json!({"namespace":"test","kind":"review","id":"invalid"});
        let invalid = store
            .reserve_external_task(
                opened.engagement_id,
                member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &accepted,
                    objective: "Check an invalid report.",
                    external_request_ref: &invalid_ref,
                    execution_intent: "isolated_write",
                    deadline_seconds: 60,
                    idempotency_key: "invalid-isolated-v3",
                },
            )
            .unwrap();
        store
            .mark_external_task_dispatching(
                opened.engagement_id,
                invalid.task_id,
                "invalid-isolated-v3",
            )
            .unwrap();
        store
            .mark_external_task_running(
                opened.engagement_id,
                invalid.task_id,
                "turn-invalid-isolated-v3",
                "invalid-isolated-v3",
            )
            .unwrap();
        let invalid_error = finish_completed_task(
            &mut store,
            &state_root,
            opened.engagement_id,
            invalid.task_id,
            member.specialist_run_id,
            &Some(FinalResponse::Inline {
                text: "not json".to_owned(),
            }),
        )
        .unwrap_err();
        assert_eq!(invalid_error.machine_error().code, "REVIEW_OUTPUT_INVALID");
        let invalid_task = store
            .external_task(opened.engagement_id, invalid.task_id)
            .unwrap();
        assert_eq!(invalid_task.state, "failed");
        assert_eq!(invalid_task.result_artifact_ref, None);
        assert_eq!(invalid_task.result, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn isolated_capture_errors_distinguish_retryable_from_permanent_failures() {
        let task_id = Uuid::now_v7();
        let transient = isolated_capture_error(task_id, WorktreePatchCaptureError::Transient);
        assert_eq!(transient.code, "OUTCOME_UNKNOWN");
        assert!(transient.retryable);
        for failure in [
            WorktreePatchCaptureError::Unavailable,
            WorktreePatchCaptureError::LimitExceeded,
        ] {
            let permanent = isolated_capture_error(task_id, failure);
            assert_eq!(permanent.code, "SPECIALIST_RESULT_INVALID");
            assert!(!permanent.retryable);
        }
    }

    #[test]
    fn queued_canonical_write_expiry_preserves_an_active_tasks_writer() {
        let fixture = crate::semantic::successor_fixture::SuccessorMember::new();
        fixture.reserve_member_writer();
        let active =
            fixture.accept_and_dispatch_task("active-writer", "canonical_workspace_write", 600);
        let mut store = fixture.store();
        store
            .transact(|tx| {
                crate::engagement::mark_external_task_running_in(
                    tx,
                    fixture.engagement_id,
                    active.task_ref,
                    "active-turn",
                    1,
                )
                .map(|_| ())
            })
            .unwrap();
        let queued = Uuid::now_v7();
        let digest = "a".repeat(64);
        store
            .transact(|tx| {
                crate::engagement::reserve_queued_external_task_in(
                    tx,
                    crate::engagement::ExternalTaskReservationCore {
                        engagement_id: fixture.engagement_id,
                        specialist_run_id: fixture.member_ref,
                        task_id: queued,
                        objective_sha256: Some(&digest),
                        external_request_json: Some("{}"),
                        request_json: "{}",
                        request_sha256: &digest,
                        execution_intent: "canonical_workspace_write",
                        deadline_seconds: 1,
                        now_ms: 1,
                    },
                    1,
                )
                .map(|_| ())
            })
            .unwrap();
        let before = fixture.writer_record();
        reconcile_engagement_with(
            &mut store,
            &fixture.view,
            &fixture.state_root,
            fixture.engagement_id,
            &fixture.owner,
            |run| {
                Ok(ControlResponseV1::Status {
                    identity: worker_identity(run),
                    lifecycle: "running".to_owned(),
                    active_turn: Some("active-turn".to_owned()),
                    last_terminal: None,
                })
            },
            |_| panic!("running and queued tasks do not need terminal recovery"),
        )
        .unwrap();
        assert_eq!(
            store
                .external_task(fixture.engagement_id, queued)
                .unwrap()
                .state,
            "expired"
        );
        assert_eq!(
            store
                .external_task(fixture.engagement_id, active.task_ref)
                .unwrap()
                .state,
            "running"
        );
        assert_eq!(fixture.writer_record(), before);
    }

    #[test]
    fn reconciliation_uses_the_ledger_only_for_quiescent_restart_recovery() {
        let root = std::env::temp_dir().join(format!(
            "dolgorae-external-reconciliation-{}",
            Uuid::now_v7()
        ));
        let state_root = root.join("state");
        let runtime = state_root.join("runtime");
        let locks = runtime.join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        for directory in [&state_root, &runtime, &locks] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let owner_path = root.join("owner.json");
        crate::controller::create_controller_credential(
            &owner_path,
            ControllerKind::Automation,
            "reconciliation-test".to_owned(),
            Some("reconciliation-principal".to_owned()),
            None,
        )
        .unwrap();
        let owner = CredentialCarrier::open_path(&owner_path).unwrap();
        let binding = binding_from_carrier(&owner, 1).unwrap();
        let canonical = root.join("workspace");
        std::fs::create_dir(&canonical).unwrap();
        let view = WorkspaceView {
            workspace_id: "a".repeat(64),
            canonical_path: crate::workspace::LosslessPath::from_path(&canonical),
            mode: WorkspaceMode::Git,
            created: false,
        };
        crate::writer::WriterStore::initialize_layout(
            &state_root,
            &view.workspace_id,
            DarwinSystem.current_uid(),
        )
        .unwrap();
        let mut store =
            EngagementStore::open(&state_root.join("orchestration/orchestration.sqlite3")).unwrap();
        let opened = store
            .open_external_engagement(
                &view.workspace_id,
                &binding,
                &json!({"namespace":"test","kind":"workflow","id":"reconcile"}),
                None,
                "open-reconcile",
            )
            .unwrap();
        let configuration = AgentConfigurationSnapshot {
            schema_version: 2,
            runtime_profile: "test".to_owned(),
            runtime_profile_snapshot_sha256: "b".repeat(64),
            model: "test-model".to_owned(),
            default_effort: "medium".to_owned(),
            purpose: Purpose {
                kind: PurposeKind::Review,
                external_label: None,
            },
            required_capabilities: vec![],
            role_reference: Some("reviewer".to_owned()),
            normalized_instructions: "Review the task.".to_owned(),
            admitted_configuration: None,
            instructions: InstructionSnapshot {
                schema: "dolgorae-instruction-snapshot/v1".to_owned(),
                common_prefix_version: 1,
                mode_prefix_version: 1,
                purpose_prefix_version: 1,
                normalized_byte_length: 16,
                normalized_sha256: "c".repeat(64),
            }
            .into(),
            execution_lane: ExecutionLane::Dedicated,
            required_assurance: Assurance::BestEffortPersonalAlpha,
            native_subagent_policy: "enabled".to_owned(),
        };
        let hire = store
            .reserve_external_hire(
                opened.engagement_id,
                "reviewer",
                &configuration,
                "Recover a terminal result.",
                "read_only",
                "hire-reconcile",
            )
            .unwrap();
        let member = store
            .finish_external_hire(&hire, RuntimeOutcome::Accepted, "hire-reconcile")
            .unwrap();
        let request = json!({"task":"recover the terminal result"});
        let external_ref = json!({"namespace":"test","kind":"task","id":"recover"});
        let task = store
            .reserve_external_task(
                opened.engagement_id,
                member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &request,
                    objective: "Recover the terminal result.",
                    external_request_ref: &external_ref,
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "task-reconcile",
                },
            )
            .unwrap();
        store
            .mark_external_task_dispatching(opened.engagement_id, task.task_id, "task-reconcile")
            .unwrap();
        store
            .mark_external_task_running(
                opened.engagement_id,
                task.task_id,
                "turn-reconcile",
                "task-reconcile",
            )
            .unwrap();
        let identity = worker_identity(member.specialist_run_id);

        reconcile_engagement_with(
            &mut store,
            &view,
            &state_root,
            opened.engagement_id,
            &owner,
            |_| {
                Ok(ControlResponseV1::Status {
                    identity: identity.clone(),
                    lifecycle: "running".to_owned(),
                    active_turn: Some("turn-reconcile".to_owned()),
                    last_terminal: None,
                })
            },
            |_| panic!("ordinary running polling must not read the durable ledger"),
        )
        .unwrap();
        assert_eq!(
            store
                .external_task(opened.engagement_id, task.task_id)
                .unwrap()
                .state,
            "running"
        );

        let integrity_error = MachineError::new(
            "AUDIT_INTEGRITY_FAILURE",
            "durable terminal is unreadable",
            false,
            json!({"run_id":member.specialist_run_id,"sequence":1,"reason":"test"}),
        );
        let observed = reconcile_engagement_with(
            &mut store,
            &view,
            &state_root,
            opened.engagement_id,
            &owner,
            |_| {
                Ok(ControlResponseV1::Status {
                    identity: identity.clone(),
                    lifecycle: "idle".to_owned(),
                    active_turn: None,
                    last_terminal: None,
                })
            },
            |_| Err(integrity_error.clone()),
        )
        .unwrap_err();
        assert_eq!(observed, integrity_error);
        assert_eq!(
            store
                .external_task(opened.engagement_id, task.task_id)
                .unwrap()
                .state,
            "running"
        );

        let terminal: crate::turn::TerminalTurn = serde_json::from_value(json!({
            "thread_id":"thread-reconcile",
            "turn_id":"turn-reconcile",
            "status":"completed",
            "effort":"medium",
            "final_response":{"kind":"inline","text":"durable answer"},
            "usage":{"inputTokens":1,"outputTokens":1}
        }))
        .unwrap();
        reconcile_engagement_with(
            &mut store,
            &view,
            &state_root,
            opened.engagement_id,
            &owner,
            |_| {
                Ok(ControlResponseV1::Status {
                    identity: identity.clone(),
                    lifecycle: "idle".to_owned(),
                    active_turn: None,
                    last_terminal: None,
                })
            },
            |_| Ok(Some(terminal.clone())),
        )
        .unwrap();
        let recovered = store
            .external_task(opened.engagement_id, task.task_id)
            .unwrap();
        assert_eq!(recovered.state, "completed_not_delivered");
        assert_eq!(
            recovered.result,
            Some(json!({"kind":"inline","text":"durable answer"}))
        );

        let mismatch_ref = json!({"namespace":"test","kind":"task","id":"mismatched-terminal"});
        let mismatch = store
            .reserve_external_task(
                opened.engagement_id,
                member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &json!({"task":"do not adopt stale terminal bytes"}),
                    objective: "Reject stale terminal recovery.",
                    external_request_ref: &mismatch_ref,
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "task-mismatched-terminal",
                },
            )
            .unwrap();
        store
            .mark_external_task_dispatching(
                opened.engagement_id,
                mismatch.task_id,
                "task-mismatched-terminal",
            )
            .unwrap();
        store
            .mark_external_task_running(
                opened.engagement_id,
                mismatch.task_id,
                "turn-current",
                "task-mismatched-terminal",
            )
            .unwrap();
        reconcile_engagement_with(
            &mut store,
            &view,
            &state_root,
            opened.engagement_id,
            &owner,
            |_| {
                Ok(ControlResponseV1::Status {
                    identity: identity.clone(),
                    lifecycle: "idle".to_owned(),
                    active_turn: None,
                    last_terminal: None,
                })
            },
            |_| Ok(Some(terminal.clone())),
        )
        .unwrap();
        let mismatch = store
            .external_task(opened.engagement_id, mismatch.task_id)
            .unwrap();
        assert_eq!(mismatch.state, "interrupted_unknown");
        assert_eq!(mismatch.result, None);

        let paused_recovery_ref = json!({"namespace":"test","kind":"task","id":"paused-recovery"});
        let paused_recovery = store
            .reserve_external_task(
                opened.engagement_id,
                member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &json!({"task":"recover a paused worker terminal"}),
                    objective: "Recover matching terminal evidence from a paused Worker.",
                    external_request_ref: &paused_recovery_ref,
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "task-paused-recovery",
                },
            )
            .unwrap();
        store
            .mark_external_task_dispatching(
                opened.engagement_id,
                paused_recovery.task_id,
                "task-paused-recovery",
            )
            .unwrap();
        store
            .mark_external_task_running(
                opened.engagement_id,
                paused_recovery.task_id,
                "turn-paused-recovery",
                "task-paused-recovery",
            )
            .unwrap();
        let paused_terminal: crate::turn::TerminalTurn = serde_json::from_value(json!({
            "thread_id":"thread-paused-recovery",
            "turn_id":"turn-paused-recovery",
            "status":"completed",
            "effort":"medium",
            "final_response":{"kind":"inline","text":"paused durable answer"},
            "usage":{"inputTokens":1,"outputTokens":1}
        }))
        .unwrap();
        reconcile_engagement_with(
            &mut store,
            &view,
            &state_root,
            opened.engagement_id,
            &owner,
            |_| {
                Ok(ControlResponseV1::Status {
                    identity: identity.clone(),
                    lifecycle: "paused".to_owned(),
                    active_turn: None,
                    last_terminal: None,
                })
            },
            |_| Ok(Some(paused_terminal.clone())),
        )
        .unwrap();
        let paused_recovery = store
            .external_task(opened.engagement_id, paused_recovery.task_id)
            .unwrap();
        assert_eq!(paused_recovery.state, "completed_not_delivered");
        assert_eq!(
            paused_recovery.result,
            Some(json!({"kind":"inline","text":"paused durable answer"}))
        );

        let paused_ref = json!({"namespace":"test","kind":"task","id":"paused"});
        let paused = store
            .reserve_external_task(
                opened.engagement_id,
                member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &json!({"task":"recover a paused worker"}),
                    objective: "Reconcile paused state without terminal evidence.",
                    external_request_ref: &paused_ref,
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "task-paused",
                },
            )
            .unwrap();
        store
            .mark_external_task_dispatching(opened.engagement_id, paused.task_id, "task-paused")
            .unwrap();
        store
            .mark_external_task_running(
                opened.engagement_id,
                paused.task_id,
                "turn-paused",
                "task-paused",
            )
            .unwrap();
        reconcile_engagement_with(
            &mut store,
            &view,
            &state_root,
            opened.engagement_id,
            &owner,
            |_| {
                Ok(ControlResponseV1::Status {
                    identity: identity.clone(),
                    lifecycle: "paused".to_owned(),
                    active_turn: None,
                    last_terminal: None,
                })
            },
            |_| Ok(None),
        )
        .unwrap();
        let paused = store
            .external_task(opened.engagement_id, paused.task_id)
            .unwrap();
        assert_eq!(paused.state, "interrupted_unknown");
        assert_eq!(
            paused.safe_error_code.as_deref(),
            Some("INTERRUPTED_UNKNOWN")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retryable_worker_failure_is_not_result_construction_pending() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-external-wait-failure-{}", Uuid::now_v7()));
        let mut store = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        let error = reconcile_wait(
            &mut store,
            &root,
            Uuid::now_v7(),
            Uuid::now_v7(),
            Ok(ControlResponseV1::Failed {
                code: "RUN_BUSY".to_owned(),
                message: "worker is busy".to_owned(),
                retryable: true,
                details: json!({
                    "run_id":Uuid::now_v7(),
                    "owner_kind":"startup"
                }),
            }),
        )
        .unwrap_err();
        assert_eq!(error.code, "RUN_BUSY");
        assert!(error.retryable);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_recovery_matches_exact_turn_and_releases_only_terminal_writers() {
        let task = ExternalTaskSnapshot {
            task_id: Uuid::now_v7(),
            specialist_run_id: Uuid::now_v7(),
            state: "running".to_owned(),
            turn_id: Some("turn-current".to_owned()),
            result_artifact_ref: None,
            result: None,
            safe_error_code: None,
            diagnostic: None,
        };
        let previous: crate::turn::TerminalTurn = serde_json::from_value(json!({
            "thread_id":"thread-1",
            "turn_id":"turn-previous",
            "status":"completed",
            "effort":"medium",
            "final_response":{"kind":"inline","text":"previous"},
            "usage":{"inputTokens":1,"outputTokens":1}
        }))
        .unwrap();
        let current: crate::turn::TerminalTurn = serde_json::from_value(json!({
            "thread_id":"thread-1",
            "turn_id":"turn-current",
            "status":"completed",
            "effort":"medium",
            "final_response":{"kind":"inline","text":"current"},
            "usage":{"inputTokens":1,"outputTokens":1}
        }))
        .unwrap();
        assert!(!terminal_matches_task(&task, &previous));
        assert!(terminal_matches_task(&task, &current));

        for active in ["accepted", "queued", "claimed", "dispatching", "running"] {
            assert!(!should_release_writer(true, active));
        }
        assert!(should_release_writer_after_completion_failure(
            true,
            "dispatching"
        ));
        assert!(!should_release_writer_after_completion_failure(
            false,
            "dispatching"
        ));
        for terminal in [
            "completed_not_delivered",
            "delivered",
            "failed",
            "interrupted_unknown",
            "cancelled",
            "expired",
        ] {
            assert!(should_release_writer(true, terminal));
            assert!(!should_release_writer(false, terminal));
        }
    }

    #[test]
    fn facade_v2_hire_rejects_the_legacy_profile_field() {
        assert!(
            serde_json::from_value::<Request>(json!({
                "operation":"hire_external_specialist",
                "engagement_id":Uuid::now_v7(),
                "role_ref":"researcher",
                "agent_configuration":{
                    "schema_version":2,
                    "runtime_profile":"default",
                    "global_profile_binding_sha256":null,
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
                "idempotency_key":"legacy-profile-field"
            }))
            .is_err()
        );
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
            diagnostic: None,
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
                "schema_version":2,
                "selected_profile":"default",
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
        serde_json::from_value::<Request>(base.clone())
            .unwrap()
            .validate()
            .unwrap();
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
