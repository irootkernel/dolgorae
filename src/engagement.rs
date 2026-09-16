//! Durable External Specialist Engagement authority.

use crate::controller::{CredentialCarrier, authorize_controller};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::run::{
    AgentConfigurationSnapshot, AggregateBinding, ControllerBinding, agent_configuration_digest,
};
use crate::specialist::{
    ReviewerRuntimePlan, output_invalid, validate_reviewer_output, validate_reviewer_output_v3,
};
use crate::task_request::{STRUCTURED_REVIEW_OUTPUT, SpecialistTaskRequest};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 3;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EngagementBarrier {
    BeforeOpenCommit,
    AfterOpenCommit,
    BeforeHireReservationCommit,
    AfterHireReservationCommit,
    BeforeHireOutcomeCommit,
    AfterHireOutcomeCommit,
    BeforeTaskReservationCommit,
    AfterTaskReservationCommit,
    BeforeTaskOutcomeCommit,
    AfterTaskOutcomeCommit,
    BeforeDeliveryCommit,
    AfterDeliveryCommit,
    BeforeLifecycleCommit,
    AfterLifecycleCommit,
    BeforeExternalOpenCommit,
    AfterExternalOpenCommit,
    BeforeExternalHireReservationCommit,
    AfterExternalHireReservationCommit,
    BeforeExternalHireOutcomeCommit,
    AfterExternalHireOutcomeCommit,
    BeforeExternalMemberResidencyCommit,
    AfterExternalMemberResidencyCommit,
    BeforeExternalTaskReservationCommit,
    AfterExternalTaskReservationCommit,
    BeforeExternalTaskDispatchCommit,
    AfterExternalTaskDispatchCommit,
    BeforeExternalTaskRunningCommit,
    AfterExternalTaskRunningCommit,
    BeforeExternalTaskPendingCommit,
    AfterExternalTaskPendingCommit,
    BeforeExternalTaskTerminalCommit,
    AfterExternalTaskTerminalCommit,
    BeforeExternalTaskCancelCommit,
    AfterExternalTaskCancelCommit,
    BeforeExternalDeliveryCommit,
    AfterExternalDeliveryCommit,
    BeforeExternalMemberReleaseCommit,
    AfterExternalMemberReleaseCommit,
    BeforeExternalCloseCommit,
    AfterExternalCloseCommit,
}

pub trait EngagementFaultInjector: Send + Sync {
    fn check(&self, barrier: EngagementBarrier) -> Result<(), EngagementFault>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngagementFault(pub EngagementBarrier);

impl std::fmt::Display for EngagementFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "engagement fault injected at {:?}", self.0)
    }
}

impl std::error::Error for EngagementFault {}

#[derive(Default)]
pub struct NoEngagementFaults;

impl EngagementFaultInjector for NoEngagementFaults {
    fn check(&self, _barrier: EngagementBarrier) -> Result<(), EngagementFault> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeOutcome {
    Accepted,
    Rejected,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngagementSnapshot {
    pub engagement_id: Uuid,
    pub workspace_id: String,
    pub external_controller_ref_sha256: String,
    pub state: String,
    pub specialist_run_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub result_sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HireReservation {
    pub engagement_id: Uuid,
    pub hire_operation_id: Uuid,
    pub specialist_run_id: Uuid,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReservation {
    pub engagement_id: Uuid,
    pub task_id: Uuid,
    pub specialist_run_id: Uuid,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_error: Option<MachineError>,
}

struct TaskExecution {
    outcome: RuntimeOutcome,
    value: Option<Value>,
    terminal_error: Option<MachineError>,
}

struct OperationCompletion<'a, T> {
    task_id: Option<Uuid>,
    safe_error_code: Option<&'a str>,
    response: &'a T,
}

impl From<(RuntimeOutcome, Option<Value>)> for TaskExecution {
    fn from((outcome, value): (RuntimeOutcome, Option<Value>)) -> Self {
        Self {
            outcome,
            value,
            terminal_error: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectedReview {
    pub artifact_id: Uuid,
    pub output: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalMemberSnapshot {
    pub specialist_run_id: Uuid,
    pub role_ref: String,
    pub membership_state: String,
    pub actor_residency: String,
    pub pending_task_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalEngagementSnapshot {
    pub engagement_id: Uuid,
    pub state: String,
    pub external_controller_ref: Value,
    pub revision: u64,
    pub specialists: Vec<ExternalMemberSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalHireReservation {
    pub engagement_id: Uuid,
    pub hire_operation_id: Uuid,
    pub specialist_run_id: Uuid,
    pub state: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalTaskSnapshot {
    pub task_id: Uuid,
    pub specialist_run_id: Uuid,
    pub state: String,
    pub turn_id: Option<String>,
    pub result_artifact_ref: Option<Uuid>,
    pub result: Option<Value>,
    pub safe_error_code: Option<String>,
}

pub struct ExternalTaskRequest<'a> {
    pub request_value: &'a Value,
    pub objective: &'a str,
    pub external_request_ref: &'a Value,
    pub execution_intent: &'a str,
    pub deadline_seconds: u64,
    pub idempotency_key: &'a str,
}

type ExternalOwnerAuthorityRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

pub struct EngagementStore {
    connection: Connection,
    faults: Arc<dyn EngagementFaultInjector>,
}

impl EngagementStore {
    #[must_use]
    pub fn workspace_database_path(state_root: &Path) -> PathBuf {
        state_root
            .join("orchestration")
            .join("orchestration.sqlite3")
    }

    pub fn open(path: &Path) -> Result<Self, MachineError> {
        Self::open_with_faults(path, Arc::new(NoEngagementFaults))
    }

    pub fn open_with_faults(
        path: &Path,
        faults: Arc<dyn EngagementFaultInjector>,
    ) -> Result<Self, MachineError> {
        if !path.is_absolute() {
            return Err(invalid(
                "database",
                "orchestration database path must be absolute",
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(internal)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(internal)?;
        }
        let mut connection = Connection::open(path).map_err(internal)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(internal)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(internal)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA synchronous=FULL;
                 CREATE TABLE IF NOT EXISTS metadata(
                   key TEXT PRIMARY KEY, value TEXT NOT NULL
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS engagements(
                   engagement_id TEXT PRIMARY KEY,
                   workspace_id TEXT NOT NULL,
                   controller_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL,
                   specialist_run_id TEXT,
                   task_id TEXT,
                   result_sha256 TEXT,
                   result_json TEXT,
                   revision INTEGER NOT NULL,
                   bootstrap_operation_id TEXT,
                   external_controller_ref_json TEXT,
                   external_controller_ref_sha256 TEXT,
                   label TEXT,
                   controller_binding_json TEXT,
                   created_at_ms INTEGER,
                   updated_at_ms INTEGER,
                   authority_kind TEXT NOT NULL DEFAULT 'legacy_one_shot'
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS operations(
                   scope_id TEXT NOT NULL DEFAULT 'legacy',
                   operation TEXT NOT NULL,
                   idempotency_key TEXT NOT NULL,
                   request_sha256 TEXT NOT NULL,
                   response_json TEXT NOT NULL,
                   PRIMARY KEY(scope_id,operation,idempotency_key)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS members(
                   specialist_run_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL,
                   hire_operation_id TEXT NOT NULL UNIQUE,
                   configuration_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL,
                   role_ref TEXT,
                   configuration_json TEXT,
                   objective_sha256 TEXT,
                   requested_access TEXT,
                   actor_residency TEXT,
                   created_at_ms INTEGER,
                   updated_at_ms INTEGER,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS tasks(
                   task_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL,
                   specialist_run_id TEXT NOT NULL,
                   objective_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL,
                   result_sha256 TEXT,
                   external_request_ref_json TEXT,
                   request_json TEXT,
                   execution_intent TEXT,
                   deadline_seconds INTEGER,
                   turn_id TEXT,
                   artifact_id TEXT,
                   safe_error_code TEXT,
                   created_at_ms INTEGER,
                   updated_at_ms INTEGER,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id),
                   FOREIGN KEY(specialist_run_id) REFERENCES members(specialist_run_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS artifacts(
                   artifact_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL,
                   task_id TEXT UNIQUE,
                   specialist_run_id TEXT,
                   result_sha256 TEXT NOT NULL,
                   canonical_json TEXT NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id),
                   FOREIGN KEY(task_id) REFERENCES tasks(task_id),
                   FOREIGN KEY(specialist_run_id) REFERENCES members(specialist_run_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS delivery_receipts(
                   sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                   task_id TEXT NOT NULL UNIQUE,
                   artifact_id TEXT NOT NULL,
                   delivered_at_ms INTEGER NOT NULL,
                   FOREIGN KEY(task_id) REFERENCES tasks(task_id),
                   FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS events(
                   sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                   engagement_id TEXT NOT NULL,
                   kind TEXT NOT NULL,
                   payload_sha256 TEXT NOT NULL,
                   previous_hash TEXT NOT NULL,
                   event_hash TEXT NOT NULL,
                   created_at_ms INTEGER NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                 ) STRICT;",
            )
            .map_err(internal)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO metadata(key,value) VALUES('schema_version',?1)",
                [SCHEMA_VERSION.to_string()],
            )
            .map_err(internal)?;
        let observed: String = connection
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if observed == "1" {
            connection
                .execute_batch("PRAGMA foreign_keys=OFF;")
                .map_err(internal)?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(internal)?;
            let locked_observed: String = transaction
                .query_row(
                    "SELECT value FROM metadata WHERE key='schema_version'",
                    [],
                    |row| row.get(0),
                )
                .map_err(internal)?;
            if locked_observed == "1" {
                transaction
                    .execute_batch(
                        "ALTER TABLE delivery_receipts RENAME TO delivery_receipts_v1;
                     ALTER TABLE artifacts RENAME TO artifacts_v1;
                     CREATE TABLE artifacts(
                       artifact_id TEXT PRIMARY KEY,
                       engagement_id TEXT NOT NULL UNIQUE,
                       result_sha256 TEXT NOT NULL,
                       canonical_json TEXT NOT NULL,
                       FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                     ) STRICT;
                     INSERT INTO artifacts(
                       artifact_id, engagement_id, result_sha256, canonical_json
                     ) SELECT
                       artifact_id, engagement_id, result_sha256, canonical_json
                     FROM artifacts_v1;
                     CREATE TABLE delivery_receipts(
                       sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                       task_id TEXT NOT NULL UNIQUE,
                       artifact_id TEXT NOT NULL,
                       delivered_at_ms INTEGER NOT NULL,
                       FOREIGN KEY(task_id) REFERENCES tasks(task_id),
                       FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                     ) STRICT;
                     INSERT INTO delivery_receipts(
                       sequence, task_id, artifact_id, delivered_at_ms
                     ) SELECT
                       sequence, task_id, artifact_id, delivered_at_ms
                     FROM delivery_receipts_v1;
                     DROP TABLE delivery_receipts_v1;
                     DROP TABLE artifacts_v1;
                     UPDATE metadata SET value='2' WHERE key='schema_version';",
                    )
                    .map_err(internal)?;
                let violation: i64 = transaction
                    .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                        row.get(0)
                    })
                    .map_err(internal)?;
                if violation != 0 {
                    return Err(internal(
                        "orchestration v1 migration violated a foreign key",
                    ));
                }
            } else if locked_observed != SCHEMA_VERSION.to_string() {
                return Err(MachineError::new(
                    "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                    "orchestration schema is not supported",
                    false,
                    serde_json::json!({"observed":locked_observed}),
                ));
            }
            transaction.commit().map_err(internal)?;
            connection
                .execute_batch("PRAGMA foreign_keys=ON;")
                .map_err(internal)?;
        } else if observed != "2" && observed != SCHEMA_VERSION.to_string() {
            return Err(MachineError::new(
                "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                "orchestration schema is not supported",
                false,
                serde_json::json!({"observed":observed}),
            ));
        }
        let observed: String = connection
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if observed == "2" {
            migrate_v2_to_v3(&mut connection)?;
        }
        connection
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS one_active_external_task_per_member
                   ON tasks(specialist_run_id)
                   WHERE state IN ('accepted','queued','claimed','dispatching','running');",
            )
            .map_err(internal)?;
        Ok(Self { connection, faults })
    }

    pub fn open_engagement(
        &mut self,
        workspace_id: &str,
        controller_sha256: &str,
        idempotency_key: &str,
    ) -> Result<EngagementSnapshot, MachineError> {
        checked(workspace_id, 256, "workspace_id")?;
        digest(controller_sha256, "controller_sha256")?;
        checked(idempotency_key, 256, "idempotency_key")?;
        let request = digest_value(&serde_json::json!({
            "workspace_id": workspace_id,
            "controller_sha256": controller_sha256
        }))?;
        if let Some(replay) =
            replay::<EngagementSnapshot>(&self.connection, "open", idempotency_key, &request)?
        {
            return Ok(replay);
        }
        let engagement_id = Uuid::now_v7();
        let result = EngagementSnapshot {
            engagement_id,
            workspace_id: workspace_id.to_owned(),
            external_controller_ref_sha256: controller_sha256.to_owned(),
            state: "open".to_owned(),
            specialist_run_id: None,
            task_id: None,
            result_sha256: None,
        };
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        transaction
            .execute(
                "INSERT INTO engagements(
                   engagement_id,workspace_id,controller_sha256,state,
                   specialist_run_id,task_id,result_sha256,result_json,revision
                 ) VALUES(?1,?2,?3,'open',NULL,NULL,NULL,NULL,1)",
                params![engagement_id.to_string(), workspace_id, controller_sha256],
            )
            .map_err(internal)?;
        append_event(&transaction, engagement_id, "opened", &request)?;
        record(&transaction, "open", idempotency_key, &request, &result)?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeOpenCommit,
            EngagementBarrier::AfterOpenCommit,
        )?;
        Ok(result)
    }

    /// Reserve all identities durably before invoking the runtime publisher.
    pub fn hire_reviewer<F>(
        &mut self,
        engagement_id: Uuid,
        plan: &ReviewerRuntimePlan,
        idempotency_key: &str,
        publish: F,
    ) -> Result<HireReservation, MachineError>
    where
        F: FnOnce(&HireReservation, &AggregateBinding, &ReviewerRuntimePlan) -> RuntimeOutcome,
    {
        let configuration_sha256 =
            agent_configuration_digest(&plan.agent_configuration).map_err(internal)?;
        self.hire(
            engagement_id,
            &configuration_sha256,
            idempotency_key,
            |reservation| {
                let binding = plan
                    .aggregate_binding(reservation.engagement_id, reservation.hire_operation_id)?;
                Ok(publish(reservation, &binding, plan))
            },
        )
    }

    fn hire<F>(
        &mut self,
        engagement_id: Uuid,
        configuration_sha256: &str,
        idempotency_key: &str,
        publish: F,
    ) -> Result<HireReservation, MachineError>
    where
        F: FnOnce(&HireReservation) -> Result<RuntimeOutcome, MachineError>,
    {
        digest(configuration_sha256, "configuration_sha256")?;
        checked(idempotency_key, 256, "idempotency_key")?;
        let request = digest_value(&serde_json::json!({
            "engagement_id": engagement_id,
            "configuration_sha256": configuration_sha256,
            "requested_access": "read_only"
        }))?;
        if let Some(mut replay) =
            replay::<HireReservation>(&self.connection, "hire", idempotency_key, &request)?
        {
            if replay.state == "provisioning" {
                replay.state = "recovery_required".to_owned();
                self.finish_operation(
                    engagement_id,
                    "recovery_required",
                    "hire_outcome_unknown_after_restart",
                    "hire",
                    idempotency_key,
                    OperationCompletion {
                        task_id: None,
                        safe_error_code: None,
                        response: &replay,
                    },
                )?;
            }
            return Ok(replay);
        }
        let hire_operation_id = Uuid::now_v7();
        let specialist_run_id = Uuid::now_v7();
        let reserved = HireReservation {
            engagement_id,
            hire_operation_id,
            specialist_run_id,
            state: "provisioning".to_owned(),
        };
        {
            let faults = Arc::clone(&self.faults);
            let transaction = self.transaction()?;
            require_current(&transaction, engagement_id, &["open"])?;
            let active: i64 = transaction
                .query_row(
                    "SELECT specialist_run_id IS NOT NULL FROM engagements WHERE engagement_id=?1",
                    [engagement_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(internal)?;
            if active != 0 {
                return Err(conflict("engagement already has a Specialist"));
            }
            transaction
                .execute(
                    "UPDATE engagements SET specialist_run_id=?2,state='provisioning',revision=revision+1 WHERE engagement_id=?1",
                    params![engagement_id.to_string(), specialist_run_id.to_string()],
                )
                .map_err(internal)?;
            transaction
                .execute(
                    "INSERT INTO members(
                       specialist_run_id,engagement_id,hire_operation_id,
                       configuration_sha256,state
                     ) VALUES(?1,?2,?3,?4,'provisioning')",
                    params![
                        specialist_run_id.to_string(),
                        engagement_id.to_string(),
                        hire_operation_id.to_string(),
                        configuration_sha256
                    ],
                )
                .map_err(internal)?;
            append_event(&transaction, engagement_id, "hire_reserved", &request)?;
            record(&transaction, "hire", idempotency_key, &request, &reserved)?;
            commit_with(
                &faults,
                transaction,
                EngagementBarrier::BeforeHireReservationCommit,
                EngagementBarrier::AfterHireReservationCommit,
            )?;
        }
        let outcome = publish(&reserved)?;
        let state = match outcome {
            RuntimeOutcome::Accepted => "ready",
            RuntimeOutcome::Rejected => "failed",
            RuntimeOutcome::Unknown => "recovery_required",
        };
        let final_result = HireReservation {
            state: state.to_owned(),
            ..reserved
        };
        self.finish_operation(
            engagement_id,
            state,
            "hire_runtime_outcome",
            "hire",
            idempotency_key,
            OperationCompletion {
                task_id: None,
                safe_error_code: None,
                response: &final_result,
            },
        )?;
        Ok(final_result)
    }

    /// Reserve one task before a Turn effect and commit its result before delivery.
    pub fn assign_review<F>(
        &mut self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        objective: &str,
        idempotency_key: &str,
        execute: F,
    ) -> Result<TaskReservation, MachineError>
    where
        F: FnOnce(&TaskReservation, &str) -> (RuntimeOutcome, Option<Value>),
    {
        checked_text(objective, 65_536, "objective")?;
        let objective_sha256 = sha256_hex(objective.as_bytes());
        self.assign(
            engagement_id,
            specialist_run_id,
            &objective_sha256,
            None,
            idempotency_key,
            |reservation| {
                let (outcome, value) = execute(reservation, objective);
                if outcome == RuntimeOutcome::Accepted {
                    match value.and_then(|value| {
                        validate_reviewer_output(value)
                            .ok()
                            .and_then(|output| serde_json::to_value(output).ok())
                    }) {
                        Some(value) => TaskExecution {
                            outcome: RuntimeOutcome::Accepted,
                            value: Some(value),
                            terminal_error: None,
                        },
                        None => TaskExecution {
                            outcome: RuntimeOutcome::Rejected,
                            value: None,
                            terminal_error: None,
                        },
                    }
                } else {
                    TaskExecution {
                        outcome,
                        value: None,
                        terminal_error: None,
                    }
                }
            },
        )
    }

    pub fn assign_task_v3<F>(
        &mut self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        accepted_request: &Value,
        task: &SpecialistTaskRequest,
        idempotency_key: &str,
        execute: F,
    ) -> Result<TaskReservation, MachineError>
    where
        F: FnOnce(
            &TaskReservation,
            &SpecialistTaskRequest,
            &str,
        ) -> Result<(RuntimeOutcome, Option<Value>), MachineError>,
    {
        task.validate()?;
        let prompt = task.prompt()?;
        let request_json = canonical_string(accepted_request)?;
        let request_sha256 = sha256_hex(request_json.as_bytes());
        let result = self.assign(
            engagement_id,
            specialist_run_id,
            &request_sha256,
            Some(&request_json),
            idempotency_key,
            |reservation| {
                let (outcome, value) = match execute(reservation, task, &prompt) {
                    Ok(execution) => execution,
                    Err(error) => {
                        return TaskExecution {
                            outcome: RuntimeOutcome::Rejected,
                            value: None,
                            terminal_error: Some(error),
                        };
                    }
                };
                if outcome == RuntimeOutcome::Accepted
                    && task.expected_output == STRUCTURED_REVIEW_OUTPUT
                {
                    let checked = value
                        .ok_or_else(|| output_invalid("Reviewer returned no structured output"))
                        .and_then(|value| validate_reviewer_output_v3(value, task))
                        .and_then(|output| serde_json::to_value(output).map_err(internal));
                    match checked {
                        Ok(value) => TaskExecution {
                            outcome: RuntimeOutcome::Accepted,
                            value: Some(value),
                            terminal_error: None,
                        },
                        Err(error) => TaskExecution {
                            outcome: RuntimeOutcome::Rejected,
                            value: None,
                            terminal_error: Some(error),
                        },
                    }
                } else {
                    TaskExecution {
                        outcome,
                        value,
                        terminal_error: None,
                    }
                }
            },
        );
        match result {
            Ok(reservation) => match reservation.terminal_error.clone() {
                Some(error) => Err(error),
                None => Ok(reservation),
            },
            Err(error) => Err(error),
        }
    }

    fn assign<F, T>(
        &mut self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        objective_sha256: &str,
        request_json: Option<&str>,
        idempotency_key: &str,
        execute: F,
    ) -> Result<TaskReservation, MachineError>
    where
        F: FnOnce(&TaskReservation) -> T,
        T: Into<TaskExecution>,
    {
        digest(objective_sha256, "objective_sha256")?;
        checked(idempotency_key, 256, "idempotency_key")?;
        let request = digest_value(&serde_json::json!({
            "engagement_id": engagement_id,
            "specialist_run_id": specialist_run_id,
            "objective_sha256": objective_sha256,
            "execution_intent": "read_only"
        }))?;
        if let Some(mut replay) =
            replay::<TaskReservation>(&self.connection, "assign", idempotency_key, &request)?
        {
            if replay.state == "accepted" {
                replay.state = "interrupted_unknown".to_owned();
                self.finish_operation(
                    engagement_id,
                    "interrupted_unknown",
                    "task_outcome_unknown_after_restart",
                    "assign",
                    idempotency_key,
                    OperationCompletion {
                        task_id: Some(replay.task_id),
                        safe_error_code: None,
                        response: &replay,
                    },
                )?;
            }
            return Ok(replay);
        }
        let deadline_seconds = request_json
            .map(|value| serde_json::from_str::<Value>(value).map_err(internal))
            .transpose()?
            .and_then(|value| value.get("deadline_seconds").and_then(Value::as_u64));
        let task_id = Uuid::now_v7();
        let created_at_ms = unix_time_ms()?;
        let reserved = TaskReservation {
            engagement_id,
            task_id,
            specialist_run_id,
            state: "accepted".to_owned(),
            terminal_error: None,
        };
        {
            let faults = Arc::clone(&self.faults);
            let transaction = self.transaction()?;
            require_current(&transaction, engagement_id, &["ready"])?;
            let expected: String = transaction
                .query_row(
                    "SELECT specialist_run_id FROM engagements WHERE engagement_id=?1",
                    [engagement_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(internal)?;
            if expected != specialist_run_id.to_string() {
                return Err(conflict("Specialist does not belong to this engagement"));
            }
            transaction
                .execute(
                    "UPDATE engagements SET task_id=?2,state='executing',revision=revision+1 WHERE engagement_id=?1 AND task_id IS NULL",
                    params![engagement_id.to_string(), task_id.to_string()],
                )
                .map_err(internal)?;
            transaction
                .execute(
                    "INSERT INTO tasks(
                           task_id,engagement_id,specialist_run_id,
                           objective_sha256,state,result_sha256,request_json,
                           deadline_seconds,created_at_ms,updated_at_ms
                     ) VALUES(?1,?2,?3,?4,'accepted',NULL,?5,?6,?7,?7)",
                    params![
                        task_id.to_string(),
                        engagement_id.to_string(),
                        specialist_run_id.to_string(),
                        objective_sha256,
                        request_json,
                        deadline_seconds
                            .map(i64::try_from)
                            .transpose()
                            .map_err(internal)?,
                        created_at_ms
                    ],
                )
                .map_err(internal)?;
            append_event(&transaction, engagement_id, "task_reserved", &request)?;
            record(&transaction, "assign", idempotency_key, &request, &reserved)?;
            commit_with(
                &faults,
                transaction,
                EngagementBarrier::BeforeTaskReservationCommit,
                EngagementBarrier::AfterTaskReservationCommit,
            )?;
        }
        let execution = execute(&reserved).into();
        match execution.outcome {
            RuntimeOutcome::Accepted => {
                let result = execution
                    .value
                    .ok_or_else(|| conflict("accepted task has no result"))?;
                let bytes = canonical_json(&result)?;
                let result_sha256 = sha256_hex(&bytes);
                let artifact_id = Uuid::now_v7();
                let canonical_json = String::from_utf8(bytes).map_err(internal)?;
                let faults = Arc::clone(&self.faults);
                let transaction = self.transaction()?;
                transaction
                    .execute(
                        "UPDATE engagements SET state='result_ready',result_sha256=?2,result_json=?3,revision=revision+1 WHERE engagement_id=?1",
                        params![engagement_id.to_string(), result_sha256, canonical_json],
                    )
                    .map_err(internal)?;
                transaction
                    .execute(
                        "INSERT INTO artifacts(
                           artifact_id,engagement_id,result_sha256,canonical_json
                         ) VALUES(?1,?2,?3,?4)",
                        params![
                            artifact_id.to_string(),
                            engagement_id.to_string(),
                            result_sha256,
                            canonical_json
                        ],
                    )
                    .map_err(internal)?;
                let changed = transaction
                    .execute(
                        "UPDATE tasks SET state='result_ready',result_sha256=?3 WHERE engagement_id=?1 AND task_id=?2",
                        params![engagement_id.to_string(), task_id.to_string(), result_sha256],
                    )
                    .map_err(internal)?;
                if changed != 1 {
                    return Err(conflict("task result target is absent or concurrent"));
                }
                append_event(
                    &transaction,
                    engagement_id,
                    "result_committed",
                    &result_sha256,
                )?;
                let final_result = TaskReservation {
                    state: "result_ready".to_owned(),
                    ..reserved
                };
                update_response(&transaction, "assign", idempotency_key, &final_result)?;
                commit_with(
                    &faults,
                    transaction,
                    EngagementBarrier::BeforeTaskOutcomeCommit,
                    EngagementBarrier::AfterTaskOutcomeCommit,
                )?;
                Ok(final_result)
            }
            RuntimeOutcome::Rejected => {
                let final_result = TaskReservation {
                    state: "failed".to_owned(),
                    terminal_error: execution.terminal_error,
                    ..reserved
                };
                let safe_error_code = final_result
                    .terminal_error
                    .as_ref()
                    .map(|error| error.code.as_str());
                self.finish_operation(
                    engagement_id,
                    "failed",
                    "task_rejected",
                    "assign",
                    idempotency_key,
                    OperationCompletion {
                        task_id: Some(final_result.task_id),
                        safe_error_code,
                        response: &final_result,
                    },
                )?;
                Ok(final_result)
            }
            RuntimeOutcome::Unknown => {
                let final_result = TaskReservation {
                    state: "interrupted_unknown".to_owned(),
                    ..reserved
                };
                self.finish_operation(
                    engagement_id,
                    "interrupted_unknown",
                    "task_outcome_unknown",
                    "assign",
                    idempotency_key,
                    OperationCompletion {
                        task_id: Some(final_result.task_id),
                        safe_error_code: None,
                        response: &final_result,
                    },
                )?;
                Ok(final_result)
            }
        }
    }

    pub fn collect(&mut self, engagement_id: Uuid) -> Result<Value, MachineError> {
        self.collect_review(engagement_id)
            .map(|collected| collected.output)
    }

    /// Deliver the immutable result together with its durable artifact identity.
    /// The compatibility `collect` method above deliberately keeps the TASK-009
    /// facade shape unchanged while the one-shot coordinator needs the reference
    /// required by its checked public result.
    pub fn collect_review(&mut self, engagement_id: Uuid) -> Result<CollectedReview, MachineError> {
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let (state, task_id, artifact_id, result): (String, Option<String>, Option<String>, Option<String>) = transaction
            .query_row(
                "SELECT e.state,e.task_id,a.artifact_id,a.canonical_json FROM engagements e LEFT JOIN artifacts a ON a.engagement_id=e.engagement_id WHERE e.engagement_id=?1",
                [engagement_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(not_found)?;
        if state != "result_ready" {
            return Err(conflict("result is not ready"));
        }
        let task_id = task_id.ok_or_else(|| conflict("result task is missing"))?;
        let artifact_id = artifact_id.ok_or_else(|| conflict("result artifact is missing"))?;
        let result = result.ok_or_else(|| conflict("result artifact is missing"))?;
        let receipt_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM delivery_receipts WHERE task_id=?1)",
                [&task_id],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if !receipt_exists {
            transaction
                .execute(
                    "INSERT INTO delivery_receipts(task_id,artifact_id,delivered_at_ms) VALUES(?1,?2,?3)",
                    params![task_id, artifact_id, unix_time_ms()?],
                )
                .map_err(internal)?;
            transaction
                .execute(
                    "UPDATE tasks SET state='delivered' WHERE task_id=?1",
                    [&task_id],
                )
                .map_err(internal)?;
            append_event(
                &transaction,
                engagement_id,
                "result_delivered",
                &artifact_id,
            )?;
        }
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeDeliveryCommit,
            EngagementBarrier::AfterDeliveryCommit,
        )?;
        Ok(CollectedReview {
            artifact_id: artifact_id.parse().map_err(internal)?,
            output: serde_json::from_str(&result).map_err(internal)?,
        })
    }

    pub fn await_terminal(
        &self,
        engagement_id: Uuid,
        timeout: Duration,
    ) -> Result<EngagementSnapshot, MachineError> {
        if timeout.is_zero() || timeout > Duration::from_secs(86_400) {
            return Err(invalid("timeout", "timeout must be between 1ns and 24h"));
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let snapshot = self.snapshot(engagement_id)?;
            if matches!(
                snapshot.state.as_str(),
                "result_ready"
                    | "failed"
                    | "cancelled"
                    | "recovery_required"
                    | "interrupted_unknown"
                    | "released"
                    | "closed"
            ) {
                return Ok(snapshot);
            }
            if std::time::Instant::now() >= deadline {
                return Err(MachineError::new(
                    "ENGAGEMENT_TIMEOUT",
                    "engagement did not reach a terminal execution state before the deadline",
                    false,
                    serde_json::json!({"required_action":"cancel_or_await_again"}),
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn task_deadline_remaining(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<Option<Duration>, MachineError> {
        let deadline: Option<(Option<i64>, Option<i64>)> = self
            .connection
            .query_row(
                "SELECT created_at_ms,deadline_seconds FROM tasks \
                 WHERE engagement_id=?1 AND task_id=?2",
                params![engagement_id.to_string(), task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((Some(created_at_ms), Some(deadline_seconds))) = deadline else {
            return Ok(None);
        };
        let deadline_ms = created_at_ms.saturating_add(deadline_seconds.saturating_mul(1_000));
        let remaining_ms = deadline_ms.saturating_sub(unix_time_ms()?).max(0);
        Ok(Some(Duration::from_millis(
            u64::try_from(remaining_ms).map_err(internal)?,
        )))
    }

    pub fn cancel(
        &mut self,
        engagement_id: Uuid,
        idempotency_key: &str,
    ) -> Result<EngagementSnapshot, MachineError> {
        let state = self.snapshot(engagement_id)?.state;
        if state == "interrupted_unknown" {
            return Err(conflict("unknown work cannot be declared cancelled"));
        }
        self.idempotent_transition(
            engagement_id,
            "cancel",
            idempotency_key,
            &["open", "provisioning", "ready", "executing", "failed"],
            "cancelled",
        )
    }

    pub fn release(
        &mut self,
        engagement_id: Uuid,
        idempotency_key: &str,
    ) -> Result<EngagementSnapshot, MachineError> {
        self.idempotent_transition(
            engagement_id,
            "release",
            idempotency_key,
            &["result_ready", "failed", "cancelled"],
            "released",
        )
    }

    pub fn close(
        &mut self,
        engagement_id: Uuid,
        idempotency_key: &str,
    ) -> Result<EngagementSnapshot, MachineError> {
        self.idempotent_transition(
            engagement_id,
            "close",
            idempotency_key,
            &["released"],
            "closed",
        )
    }

    pub fn snapshot(&self, engagement_id: Uuid) -> Result<EngagementSnapshot, MachineError> {
        snapshot_in(&self.connection, engagement_id)
    }

    pub fn open_external_engagement(
        &mut self,
        workspace_id: &str,
        binding: &ControllerBinding,
        external_controller_ref: &Value,
        label: Option<&str>,
        idempotency_key: &str,
    ) -> Result<ExternalEngagementSnapshot, MachineError> {
        checked(workspace_id, 512, "workspace_id")?;
        checked(idempotency_key, 256, "idempotency_key")?;
        if binding.identity.generation != 1
            || !matches!(
                binding.identity.kind.as_str(),
                "workflow_orchestrator" | "automation"
            )
        {
            return Err(invalid(
                "controller",
                "engagement owner must be a generation-1 workflow_orchestrator or automation Controller",
            ));
        }
        if let Some(label) = label {
            checked(label, 256, "label")?;
        }
        let external_json = canonical_string(external_controller_ref)?;
        let external_sha256 = sha256_hex(external_json.as_bytes());
        let binding_json = canonical_string(&serde_json::to_value(binding).map_err(internal)?)?;
        let request = digest_value(&serde_json::json!({
            "workspace_id": workspace_id,
            "controller_binding": binding,
            "external_controller_ref": external_controller_ref,
            "label": label,
        }))?;
        let scope = format!("workspace:{workspace_id}");
        if let Some(replay) = scoped_replay::<ExternalEngagementSnapshot>(
            &self.connection,
            &scope,
            "open_external_engagement",
            idempotency_key,
            &request,
        )? {
            return Ok(replay);
        }
        let engagement_id = Uuid::now_v7();
        let bootstrap_operation_id = Uuid::now_v7();
        let now = unix_time_ms()?;
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        if let Some(replay) = scoped_replay::<ExternalEngagementSnapshot>(
            &transaction,
            &scope,
            "open_external_engagement",
            idempotency_key,
            &request,
        )? {
            return Ok(replay);
        }
        transaction
            .execute(
                "INSERT INTO engagements(
               engagement_id,workspace_id,controller_sha256,state,revision,
               bootstrap_operation_id,external_controller_ref_json,
               external_controller_ref_sha256,label,controller_binding_json,
               created_at_ms,updated_at_ms,authority_kind
             ) VALUES(?1,?2,?3,'active',1,?4,?5,?6,?7,?8,?9,?9,'external_v1')",
                params![
                    engagement_id.to_string(),
                    workspace_id,
                    sha256_hex(binding_json.as_bytes()),
                    bootstrap_operation_id.to_string(),
                    external_json,
                    external_sha256,
                    label,
                    binding_json,
                    now,
                ],
            )
            .map_err(internal)?;
        let bootstrap_payload = format!(
            "{bootstrap_operation_id}\0{external_sha256}\0{}",
            sha256_hex(binding_json.as_bytes())
        );
        append_event(
            &transaction,
            engagement_id,
            "external_engagement_opened",
            &bootstrap_payload,
        )?;
        let result = ExternalEngagementSnapshot {
            engagement_id,
            state: "active".to_owned(),
            external_controller_ref: external_controller_ref.clone(),
            revision: 1,
            specialists: Vec::new(),
        };
        scoped_record(
            &transaction,
            &scope,
            "open_external_engagement",
            idempotency_key,
            &request,
            &result,
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalOpenCommit,
            EngagementBarrier::AfterExternalOpenCommit,
        )?;
        Ok(result)
    }

    pub fn authorize_external_owner(
        &self,
        workspace_id: &str,
        engagement_id: Uuid,
        operation: &str,
        carrier: &CredentialCarrier,
    ) -> Result<ControllerBinding, MachineError> {
        let (
            recorded_workspace,
            binding_json,
            external_json,
            external_sha256,
            bootstrap,
            label,
            recorded_binding_sha256,
            kind,
        ): ExternalOwnerAuthorityRow = self
            .connection
            .query_row(
                "SELECT workspace_id,controller_binding_json,external_controller_ref_json,
                    external_controller_ref_sha256,bootstrap_operation_id,label,
                    controller_sha256,authority_kind
             FROM engagements WHERE engagement_id=?1",
                [engagement_id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(not_found)?;
        if kind != "external_v1" || recorded_workspace != workspace_id {
            return Err(not_found());
        }
        let binding_json = binding_json.ok_or_else(|| integrity("owner binding is missing"))?;
        let binding: ControllerBinding = serde_json::from_str(&binding_json)
            .map_err(|_| integrity("owner binding is invalid"))?;
        if recorded_binding_sha256 != sha256_hex(binding_json.as_bytes()) {
            return Err(integrity("owner binding digest does not match"));
        }
        let external_json =
            external_json.ok_or_else(|| integrity("external provenance is missing"))?;
        let external_sha256 =
            external_sha256.ok_or_else(|| integrity("external provenance digest is missing"))?;
        if external_sha256 != sha256_hex(external_json.as_bytes()) {
            return Err(integrity("external provenance digest does not match"));
        }
        let bootstrap = bootstrap.ok_or_else(|| integrity("bootstrap operation is missing"))?;
        let bootstrap_payload = format!(
            "{bootstrap}\0{}\0{recorded_binding_sha256}",
            external_sha256
        );
        let event_rows = self
            .connection
            .query_row(
                "SELECT COUNT(*),MIN(previous_hash),MIN(event_hash) FROM events
                 WHERE engagement_id=?1 AND kind='external_engagement_opened'
                   AND payload_sha256=?2",
                params![
                    engagement_id.to_string(),
                    sha256_hex(bootstrap_payload.as_bytes())
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .map_err(internal)?;
        let (1, Some(previous_hash), Some(event_hash)) = event_rows else {
            return Err(integrity("external engagement bootstrap event is missing"));
        };
        let expected_event_hash = event_digest(
            &previous_hash,
            engagement_id,
            "external_engagement_opened",
            &sha256_hex(bootstrap_payload.as_bytes()),
        );
        if event_hash != expected_event_hash {
            return Err(integrity(
                "external engagement bootstrap event digest does not match",
            ));
        }
        let open_request = digest_value(&serde_json::json!({
            "workspace_id": recorded_workspace,
            "controller_binding": binding,
            "external_controller_ref": serde_json::from_str::<Value>(&external_json)
                .map_err(|_| integrity("external provenance is invalid"))?,
            "label": label,
        }))?;
        let receipts: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM operations WHERE scope_id=?1
             AND operation='open_external_engagement' AND request_sha256=?2
             AND json_extract(response_json,'$.engagement_id')=?3",
                params![
                    format!("workspace:{workspace_id}"),
                    open_request,
                    engagement_id.to_string()
                ],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if receipts != 1 {
            return Err(integrity(
                "bootstrap operation receipt is missing or ambiguous",
            ));
        }
        authorize_controller(engagement_id, operation, &binding, carrier)?;
        Ok(binding)
    }

    pub fn external_snapshot(
        &self,
        engagement_id: Uuid,
    ) -> Result<ExternalEngagementSnapshot, MachineError> {
        let (state, external, revision, kind): (String, Option<String>, i64, String) = self
            .connection
            .query_row(
                "SELECT state,external_controller_ref_json,revision,authority_kind
                 FROM engagements WHERE engagement_id=?1",
                [engagement_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(not_found)?;
        if kind != "external_v1" {
            return Err(not_found());
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT m.specialist_run_id,m.role_ref,m.state,m.actor_residency,
                    COUNT(t.task_id)
             FROM members m LEFT JOIN tasks t ON t.specialist_run_id=m.specialist_run_id
               AND t.state IN ('accepted','queued','claimed','dispatching','running')
             WHERE m.engagement_id=?1
             GROUP BY m.specialist_run_id,m.role_ref,m.state,m.actor_residency
             ORDER BY m.rowid",
            )
            .map_err(internal)?;
        let specialists = statement
            .query_map([engagement_id.to_string()], |row| {
                let id: String = row.get(0)?;
                Ok(ExternalMemberSnapshot {
                    specialist_run_id: id.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
                    role_ref: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    membership_state: row.get(2)?,
                    actor_residency: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "unavailable".to_owned()),
                    pending_task_count: row.get::<_, i64>(4)?.try_into().unwrap_or(0),
                })
            })
            .map_err(internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)?;
        Ok(ExternalEngagementSnapshot {
            engagement_id,
            state,
            external_controller_ref: serde_json::from_str(
                external
                    .as_deref()
                    .ok_or_else(|| integrity("external provenance is missing"))?,
            )
            .map_err(internal)?,
            revision: revision.try_into().map_err(internal)?,
            specialists,
        })
    }

    pub fn reserve_external_hire(
        &mut self,
        engagement_id: Uuid,
        role_ref: &str,
        configuration: &AgentConfigurationSnapshot,
        objective: &str,
        requested_access: &str,
        idempotency_key: &str,
    ) -> Result<ExternalHireReservation, MachineError> {
        checked(role_ref, 64, "role_ref")?;
        checked_text(objective, 65_536, "objective")?;
        checked(idempotency_key, 256, "idempotency_key")?;
        if !matches!(
            requested_access,
            "read_only" | "isolated_write" | "canonical_workspace_write"
        ) {
            return Err(invalid("requested_access", "unsupported Specialist access"));
        }
        let configuration_json =
            canonical_string(&serde_json::to_value(configuration).map_err(internal)?)?;
        let configuration_sha256 = agent_configuration_digest(configuration).map_err(internal)?;
        let objective_sha256 = sha256_hex(objective.as_bytes());
        let request = digest_value(&serde_json::json!({
            "engagement_id": engagement_id, "role_ref": role_ref,
            "agent_configuration": configuration, "objective_sha256": objective_sha256,
            "requested_access": requested_access,
        }))?;
        let scope = engagement_id.to_string();
        if let Some(replay) = scoped_replay(
            &self.connection,
            &scope,
            "hire_external_specialist",
            idempotency_key,
            &request,
        )? {
            return Ok(replay);
        }
        let hire_operation_id = Uuid::now_v7();
        let specialist_run_id = Uuid::now_v7();
        let now = unix_time_ms()?;
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        if let Some(replay) = scoped_replay(
            &transaction,
            &scope,
            "hire_external_specialist",
            idempotency_key,
            &request,
        )? {
            return Ok(replay);
        }
        require_current(
            &transaction,
            engagement_id,
            &["active", "degraded", "recovering"],
        )?;
        transaction
            .execute(
                "INSERT INTO members(
               specialist_run_id,engagement_id,hire_operation_id,
               configuration_sha256,state,role_ref,configuration_json,
               objective_sha256,requested_access,actor_residency,created_at_ms,updated_at_ms
             ) VALUES(?1,?2,?3,?4,'provisioning',?5,?6,?7,?8,'unstarted',?9,?9)",
                params![
                    specialist_run_id.to_string(),
                    engagement_id.to_string(),
                    hire_operation_id.to_string(),
                    configuration_sha256,
                    role_ref,
                    configuration_json,
                    objective_sha256,
                    requested_access,
                    now
                ],
            )
            .map_err(internal)?;
        transaction.execute(
            "UPDATE engagements SET revision=revision+1,updated_at_ms=?2 WHERE engagement_id=?1",
            params![engagement_id.to_string(), now],
        ).map_err(internal)?;
        let result = ExternalHireReservation {
            engagement_id,
            hire_operation_id,
            specialist_run_id,
            state: "provisioning".to_owned(),
        };
        append_event(
            &transaction,
            engagement_id,
            "external_hire_reserved",
            &request,
        )?;
        scoped_record(
            &transaction,
            &scope,
            "hire_external_specialist",
            idempotency_key,
            &request,
            &result,
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalHireReservationCommit,
            EngagementBarrier::AfterExternalHireReservationCommit,
        )?;
        Ok(result)
    }

    pub fn finish_external_hire(
        &mut self,
        reservation: &ExternalHireReservation,
        outcome: RuntimeOutcome,
        idempotency_key: &str,
    ) -> Result<ExternalHireReservation, MachineError> {
        let state = match outcome {
            RuntimeOutcome::Accepted => "active",
            RuntimeOutcome::Rejected => "degraded",
            RuntimeOutcome::Unknown => "degraded",
        };
        let response_state = match outcome {
            RuntimeOutcome::Accepted => "ready",
            RuntimeOutcome::Rejected | RuntimeOutcome::Unknown => "recovery_required",
        };
        let now = unix_time_ms()?;
        let result = ExternalHireReservation {
            state: response_state.to_owned(),
            ..reservation.clone()
        };
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE members SET state=?2,actor_residency=?3,updated_at_ms=?4
             WHERE specialist_run_id=?1 AND state='provisioning'",
                params![
                    reservation.specialist_run_id.to_string(),
                    state,
                    if outcome == RuntimeOutcome::Accepted {
                        "unstarted"
                    } else if outcome == RuntimeOutcome::Unknown {
                        "recovering"
                    } else {
                        "unavailable"
                    },
                    now
                ],
            )
            .map_err(internal)?;
        if changed != 1 {
            let matching_receipt: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM operations
                     WHERE scope_id=?1 AND operation='hire_external_specialist'
                       AND idempotency_key=?2
                       AND json_extract(response_json,'$.specialist_run_id')=?3)",
                    params![
                        reservation.engagement_id.to_string(),
                        idempotency_key,
                        reservation.specialist_run_id.to_string()
                    ],
                    |row| row.get(0),
                )
                .map_err(internal)?;
            if !matching_receipt {
                return Err(MachineError::new(
                    "IDEMPOTENCY_CONFLICT",
                    "hire settlement key does not match its durable reservation",
                    false,
                    serde_json::json!({"required_action":"use_original_input_or_new_key"}),
                ));
            }
            let recorded_state: String = transaction
                .query_row(
                    "SELECT state FROM members WHERE engagement_id=?1 AND specialist_run_id=?2",
                    params![
                        reservation.engagement_id.to_string(),
                        reservation.specialist_run_id.to_string()
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(internal)?
                .ok_or_else(|| specialist_not_member(reservation.specialist_run_id))?;
            return Ok(ExternalHireReservation {
                state: if recorded_state == "active" {
                    "ready".to_owned()
                } else {
                    "recovery_required".to_owned()
                },
                ..reservation.clone()
            });
        }
        if outcome != RuntimeOutcome::Accepted {
            transaction.execute(
                "UPDATE engagements SET state='degraded',revision=revision+1,updated_at_ms=?2 WHERE engagement_id=?1",
                params![reservation.engagement_id.to_string(), now],
            ).map_err(internal)?;
        }
        scoped_update_response(
            &transaction,
            &reservation.engagement_id.to_string(),
            "hire_external_specialist",
            idempotency_key,
            &result,
        )?;
        append_event(
            &transaction,
            reservation.engagement_id,
            "external_hire_outcome",
            response_state,
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalHireOutcomeCommit,
            EngagementBarrier::AfterExternalHireOutcomeCommit,
        )?;
        Ok(result)
    }

    pub fn mark_external_member_resident(
        &mut self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> Result<(), MachineError> {
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE members SET actor_residency='resident',updated_at_ms=?3
                 WHERE engagement_id=?1 AND specialist_run_id=?2
                   AND state IN ('active','degraded')",
                params![
                    engagement_id.to_string(),
                    specialist_run_id.to_string(),
                    unix_time_ms()?
                ],
            )
            .map_err(internal)?;
        if changed != 1 {
            return Err(specialist_not_member(specialist_run_id));
        }
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalMemberResidencyCommit,
            EngagementBarrier::AfterExternalMemberResidencyCommit,
        )?;
        Ok(())
    }

    pub fn external_member_configuration(
        &self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> Result<(AgentConfigurationSnapshot, String, String), MachineError> {
        let row: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT configuration_json,requested_access,state FROM members
             WHERE engagement_id=?1 AND specialist_run_id=?2",
                params![engagement_id.to_string(), specialist_run_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(internal)?;
        let (configuration, access, state) =
            row.ok_or_else(|| specialist_not_member(specialist_run_id))?;
        Ok((
            serde_json::from_str(&configuration).map_err(internal)?,
            access,
            state,
        ))
    }

    pub fn external_member_actor_residency(
        &self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> Result<String, MachineError> {
        self.connection
            .query_row(
                "SELECT actor_residency FROM members
                 WHERE engagement_id=?1 AND specialist_run_id=?2",
                params![engagement_id.to_string(), specialist_run_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?
            .flatten()
            .ok_or_else(|| specialist_not_member(specialist_run_id))
    }

    pub fn stale_external_hires(
        &self,
        engagement_id: Uuid,
        grace: Duration,
    ) -> Result<Vec<(ExternalHireReservation, String)>, MachineError> {
        let cutoff =
            unix_time_ms()?.saturating_sub(i64::try_from(grace.as_millis()).unwrap_or(i64::MAX));
        let mut statement = self
            .connection
            .prepare(
                "SELECT m.hire_operation_id,m.specialist_run_id,o.idempotency_key
                 FROM members m JOIN operations o
                   ON o.scope_id=m.engagement_id
                  AND o.operation='hire_external_specialist'
                  AND json_extract(o.response_json,'$.specialist_run_id')=m.specialist_run_id
                 WHERE m.engagement_id=?1 AND m.state='provisioning'
                   AND m.updated_at_ms<=?2
                 ORDER BY m.created_at_ms,m.specialist_run_id",
            )
            .map_err(internal)?;
        statement
            .query_map(params![engagement_id.to_string(), cutoff], |row| {
                let hire_operation_id: String = row.get(0)?;
                let specialist_run_id: String = row.get(1)?;
                Ok((
                    ExternalHireReservation {
                        engagement_id,
                        hire_operation_id: hire_operation_id
                            .parse()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        specialist_run_id: specialist_run_id
                            .parse()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        state: "provisioning".to_owned(),
                    },
                    row.get(2)?,
                ))
            })
            .map_err(internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)
    }

    pub fn external_canonical_members_without_active_tasks(
        &self,
        engagement_id: Uuid,
    ) -> Result<Vec<Uuid>, MachineError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT m.specialist_run_id FROM members m
                 WHERE m.engagement_id=?1
                   AND m.state IN ('active','degraded')
                   AND m.requested_access='canonical_workspace_write'
                   AND NOT EXISTS(
                     SELECT 1 FROM tasks t
                     WHERE t.engagement_id=m.engagement_id
                       AND t.specialist_run_id=m.specialist_run_id
                       AND t.state IN ('accepted','queued','claimed','dispatching','running')
                   )
                 ORDER BY m.specialist_run_id",
            )
            .map_err(internal)?;
        statement
            .query_map([engagement_id.to_string()], |row| {
                let run_id: String = row.get(0)?;
                run_id.parse().map_err(|_| rusqlite::Error::InvalidQuery)
            })
            .map_err(internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)
    }

    pub fn reserve_external_task(
        &mut self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        request: ExternalTaskRequest<'_>,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        let ExternalTaskRequest {
            request_value,
            objective,
            external_request_ref,
            execution_intent,
            deadline_seconds,
            idempotency_key,
        } = request;
        checked_text(objective, 65_536, "objective")?;
        checked(idempotency_key, 256, "idempotency_key")?;
        if !(1..=86_400).contains(&deadline_seconds) {
            return Err(invalid(
                "deadline_seconds",
                "deadline must be between 1 and 86400 seconds",
            ));
        }
        let request_json = canonical_string(request_value)?;
        let request_sha256 = sha256_hex(request_json.as_bytes());
        let external_json = canonical_string(external_request_ref)?;
        let scope = engagement_id.to_string();
        if let Some(replay) = scoped_replay::<ExternalTaskSnapshot>(
            &self.connection,
            &scope,
            "assign_external_specialist_task",
            idempotency_key,
            &request_sha256,
        )? {
            return self.external_task(engagement_id, replay.task_id);
        }
        let task_id = Uuid::now_v7();
        let now = unix_time_ms()?;
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        if let Some(replay) = scoped_replay::<ExternalTaskSnapshot>(
            &transaction,
            &scope,
            "assign_external_specialist_task",
            idempotency_key,
            &request_sha256,
        )? {
            return external_task_in(&transaction, engagement_id, replay.task_id);
        }
        require_current(
            &transaction,
            engagement_id,
            &["active", "degraded", "recovering"],
        )?;
        let (member_state, requested_access): (String, Option<String>) = transaction.query_row(
            "SELECT state,requested_access FROM members WHERE engagement_id=?1 AND specialist_run_id=?2",
            params![engagement_id.to_string(), specialist_run_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(internal)?.ok_or_else(|| specialist_not_member(specialist_run_id))?;
        if member_state != "active" {
            return Err(conflict("Specialist membership is not active"));
        }
        if !access_allows(
            requested_access.as_deref().unwrap_or("read_only"),
            execution_intent,
        ) {
            return Err(policy_denied(
                "task execution intent exceeds the hired access",
            ));
        }
        let inserted = transaction.execute(
            "INSERT INTO tasks(
               task_id,engagement_id,specialist_run_id,objective_sha256,state,
               external_request_ref_json,request_json,execution_intent,
               deadline_seconds,created_at_ms,updated_at_ms
             ) VALUES(?1,?2,?3,?4,'accepted',?5,?6,?7,?8,?9,?9)",
            params![
                task_id.to_string(),
                engagement_id.to_string(),
                specialist_run_id.to_string(),
                sha256_hex(objective.as_bytes()),
                external_json,
                request_json,
                execution_intent,
                i64::try_from(deadline_seconds).map_err(internal)?,
                now
            ],
        );
        if let Err(error) = inserted {
            let active: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks
                     WHERE specialist_run_id=?1
                       AND state IN ('accepted','queued','claimed','dispatching','running'))",
                    [specialist_run_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(internal)?;
            if active {
                return Err(conflict("Specialist already has an active task"));
            }
            return Err(internal(error));
        }
        let result = ExternalTaskSnapshot {
            task_id,
            specialist_run_id,
            state: "accepted".to_owned(),
            turn_id: None,
            result_artifact_ref: None,
            result: None,
            safe_error_code: None,
        };
        transaction.execute(
            "UPDATE engagements SET revision=revision+1,updated_at_ms=?2 WHERE engagement_id=?1",
            params![engagement_id.to_string(),now],
        ).map_err(internal)?;
        append_event(
            &transaction,
            engagement_id,
            "external_task_reserved",
            &request_sha256,
        )?;
        scoped_record(
            &transaction,
            &scope,
            "assign_external_specialist_task",
            idempotency_key,
            &request_sha256,
            &result,
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalTaskReservationCommit,
            EngagementBarrier::AfterExternalTaskReservationCommit,
        )?;
        Ok(result)
    }

    pub fn mark_external_task_running(
        &mut self,
        engagement_id: Uuid,
        task_id: Uuid,
        turn_id: &str,
        idempotency_key: &str,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        checked(turn_id, 256, "turn_id")?;
        let current = self.external_task(engagement_id, task_id)?;
        if current.state == "running" && current.turn_id.as_deref() == Some(turn_id) {
            return Ok(current);
        }
        let now = unix_time_ms()?;
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE tasks SET state='running',turn_id=?3,updated_at_ms=?4
             WHERE engagement_id=?1 AND task_id=?2 AND state='dispatching'",
                params![engagement_id.to_string(), task_id.to_string(), turn_id, now],
            )
            .map_err(internal)?;
        if changed != 1 {
            return Err(conflict("task is not at the accepted dispatch boundary"));
        }
        let result = external_task_in(&transaction, engagement_id, task_id)?;
        scoped_update_response(
            &transaction,
            &engagement_id.to_string(),
            "assign_external_specialist_task",
            idempotency_key,
            &result,
        )?;
        append_event(
            &transaction,
            engagement_id,
            "external_task_running",
            turn_id,
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalTaskRunningCommit,
            EngagementBarrier::AfterExternalTaskRunningCommit,
        )?;
        Ok(result)
    }

    pub fn mark_external_task_result_pending(
        &mut self,
        engagement_id: Uuid,
        task_id: Uuid,
        safe_error_code: &str,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        checked(safe_error_code, 128, "safe_error_code")?;
        let current = self.external_task(engagement_id, task_id)?;
        if current.state != "running" {
            if matches!(
                current.state.as_str(),
                "completed_not_delivered"
                    | "delivered"
                    | "failed"
                    | "interrupted_unknown"
                    | "cancelled"
                    | "expired"
            ) {
                return Ok(current);
            }
            return Err(conflict("task is not running result construction"));
        }
        if current.safe_error_code.as_deref() == Some(safe_error_code) {
            return Ok(current);
        }
        let now = unix_time_ms()?;
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE tasks SET safe_error_code=?3,updated_at_ms=?4
                 WHERE engagement_id=?1 AND task_id=?2 AND state='running'",
                params![
                    engagement_id.to_string(),
                    task_id.to_string(),
                    safe_error_code,
                    now
                ],
            )
            .map_err(internal)?;
        if changed != 1 {
            return external_task_in(&transaction, engagement_id, task_id);
        }
        transaction.execute(
            "UPDATE engagements SET revision=revision+1,updated_at_ms=?2 WHERE engagement_id=?1",
            params![engagement_id.to_string(), now],
        ).map_err(internal)?;
        append_event(
            &transaction,
            engagement_id,
            "external_task_result_pending",
            safe_error_code,
        )?;
        let result = external_task_in(&transaction, engagement_id, task_id)?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalTaskPendingCommit,
            EngagementBarrier::AfterExternalTaskPendingCommit,
        )?;
        Ok(result)
    }

    pub fn mark_external_task_dispatching(
        &mut self,
        engagement_id: Uuid,
        task_id: Uuid,
        idempotency_key: &str,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE tasks SET state='dispatching',updated_at_ms=?3
             WHERE engagement_id=?1 AND task_id=?2 AND state='accepted'",
                params![
                    engagement_id.to_string(),
                    task_id.to_string(),
                    unix_time_ms()?
                ],
            )
            .map_err(internal)?;
        if changed != 1 {
            let current = external_task_in(&transaction, engagement_id, task_id)?;
            if current.state == "dispatching" {
                return Ok(current);
            }
            return Err(conflict("task is not at the accepted dispatch boundary"));
        }
        let result = external_task_in(&transaction, engagement_id, task_id)?;
        scoped_update_response(
            &transaction,
            &engagement_id.to_string(),
            "assign_external_specialist_task",
            idempotency_key,
            &result,
        )?;
        append_event(
            &transaction,
            engagement_id,
            "external_task_dispatching",
            &task_id.to_string(),
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalTaskDispatchCommit,
            EngagementBarrier::AfterExternalTaskDispatchCommit,
        )?;
        Ok(result)
    }

    pub fn finish_external_task(
        &mut self,
        engagement_id: Uuid,
        task_id: Uuid,
        output: Option<&Value>,
        state: &str,
        safe_error_code: Option<&str>,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        if !matches!(
            state,
            "completed_not_delivered" | "failed" | "interrupted_unknown" | "cancelled" | "expired"
        ) {
            return Err(invalid("state", "task outcome is not terminal"));
        }
        let now = unix_time_ms()?;
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let current = external_task_in(&transaction, engagement_id, task_id)?;
        if !matches!(
            current.state.as_str(),
            "accepted" | "queued" | "claimed" | "dispatching" | "running"
        ) {
            return Ok(current);
        }
        let (artifact_id, result_sha256) = if state == "completed_not_delivered" {
            let output = output.ok_or_else(|| conflict("completed task has no result"))?;
            let canonical = canonical_string(output)?;
            let digest = sha256_hex(canonical.as_bytes());
            let artifact_id = Uuid::now_v7();
            let specialist: String = transaction
                .query_row(
                    "SELECT specialist_run_id FROM tasks WHERE engagement_id=?1 AND task_id=?2",
                    params![engagement_id.to_string(), task_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(internal)?;
            transaction.execute(
                "INSERT INTO artifacts(artifact_id,engagement_id,task_id,specialist_run_id,result_sha256,canonical_json)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![artifact_id.to_string(),engagement_id.to_string(),task_id.to_string(),specialist,digest,canonical],
            ).map_err(internal)?;
            (Some(artifact_id), Some(digest))
        } else {
            (None, None)
        };
        let changed = transaction.execute(
            "UPDATE tasks SET state=?3,result_sha256=?4,artifact_id=?5,safe_error_code=?6,updated_at_ms=?7
             WHERE engagement_id=?1 AND task_id=?2 AND state IN ('accepted','queued','claimed','dispatching','running')",
            params![engagement_id.to_string(),task_id.to_string(),state,result_sha256,
                artifact_id.map(|value| value.to_string()),safe_error_code,now],
        ).map_err(internal)?;
        if changed != 1 {
            return external_task_in(&transaction, engagement_id, task_id);
        }
        append_event(&transaction, engagement_id, "external_task_terminal", state)?;
        let result = external_task_in(&transaction, engagement_id, task_id)?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalTaskTerminalCommit,
            EngagementBarrier::AfterExternalTaskTerminalCommit,
        )?;
        Ok(result)
    }

    pub fn cancel_external_task(
        &mut self,
        engagement_id: Uuid,
        task_id: Uuid,
        terminal_state: &str,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        if !matches!(terminal_state, "cancelled" | "interrupted_unknown") {
            return Err(invalid("state", "cancellation outcome is invalid"));
        }
        checked(idempotency_key, 256, "idempotency_key")?;
        checked_text(reason, 1024, "reason")?;
        let request = digest_value(&serde_json::json!({
            "engagement_id": engagement_id,
            "task_id": task_id,
            "reason": reason,
        }))?;
        let scope = engagement_id.to_string();
        if let Some(replay) = scoped_replay(
            &self.connection,
            &scope,
            "cancel_external_specialist_task",
            idempotency_key,
            &request,
        )? {
            return Ok(replay);
        }
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        if let Some(replay) = scoped_replay(
            &transaction,
            &scope,
            "cancel_external_specialist_task",
            idempotency_key,
            &request,
        )? {
            return Ok(replay);
        }
        let current = external_task_in(&transaction, engagement_id, task_id)?;
        let result = if matches!(
            current.state.as_str(),
            "completed_not_delivered"
                | "delivered"
                | "failed"
                | "interrupted_unknown"
                | "cancelled"
                | "expired"
        ) {
            current
        } else {
            transaction
                .execute(
                    "UPDATE tasks SET state=?3,safe_error_code=?4,updated_at_ms=?5
                 WHERE engagement_id=?1 AND task_id=?2",
                    params![
                        engagement_id.to_string(),
                        task_id.to_string(),
                        terminal_state,
                        if terminal_state == "interrupted_unknown" {
                            Some("INTERRUPTED_UNKNOWN")
                        } else {
                            None
                        },
                        unix_time_ms()?
                    ],
                )
                .map_err(internal)?;
            append_event(
                &transaction,
                engagement_id,
                "external_task_cancelled",
                terminal_state,
            )?;
            external_task_in(&transaction, engagement_id, task_id)?
        };
        scoped_record(
            &transaction,
            &scope,
            "cancel_external_specialist_task",
            idempotency_key,
            &request,
            &result,
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalTaskCancelCommit,
            EngagementBarrier::AfterExternalTaskCancelCommit,
        )?;
        Ok(result)
    }

    pub fn external_task(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        external_task_in(&self.connection, engagement_id, task_id)
    }

    pub fn external_task_execution_intent(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<String, MachineError> {
        self.connection
            .query_row(
                "SELECT execution_intent FROM tasks WHERE engagement_id=?1 AND task_id=?2",
                params![engagement_id.to_string(), task_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(|| {
                MachineError::new(
                    "SPECIALIST_TASK_NOT_FOUND",
                    "Specialist task was not found",
                    false,
                    serde_json::json!({"task_id":task_id}),
                )
            })
    }

    pub fn external_task_request(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<Value, MachineError> {
        let (request, receipt_count, recorded_sha256): (Option<String>, i64, Option<String>) = self
            .connection
            .query_row(
                "SELECT t.request_json,
                   (SELECT COUNT(*) FROM operations o
                    WHERE o.scope_id=t.engagement_id
                      AND o.operation='assign_external_specialist_task'
                      AND json_extract(o.response_json,'$.task_id')=t.task_id),
                   (SELECT MIN(o.request_sha256) FROM operations o
                    WHERE o.scope_id=t.engagement_id
                      AND o.operation='assign_external_specialist_task'
                      AND json_extract(o.response_json,'$.task_id')=t.task_id)
                 FROM tasks t WHERE t.engagement_id=?1 AND t.task_id=?2",
                params![engagement_id.to_string(), task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(|| {
                MachineError::new(
                    "SPECIALIST_TASK_NOT_FOUND",
                    "Specialist task was not found",
                    false,
                    serde_json::json!({"task_id":task_id}),
                )
            })?;
        if receipt_count != 1 {
            return Err(integrity(
                "external task assignment receipt is missing or ambiguous",
            ));
        }
        let request = request.ok_or_else(|| integrity("external task request is missing"))?;
        let recorded_sha256 = recorded_sha256
            .ok_or_else(|| integrity("external task assignment receipt lost its request digest"))?;
        if sha256_hex(request.as_bytes()) != recorded_sha256 {
            return Err(integrity(
                "external task request digest does not match its assignment receipt",
            ));
        }
        serde_json::from_str(&request)
            .map_err(|_| integrity("external task request is not valid JSON"))
    }

    pub fn external_task_deadline_expired(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<bool, MachineError> {
        self.connection
            .query_row(
                "SELECT created_at_ms + deadline_seconds * 1000 <= ?3 FROM tasks
             WHERE engagement_id=?1 AND task_id=?2",
                params![
                    engagement_id.to_string(),
                    task_id.to_string(),
                    unix_time_ms()?
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(|| {
                MachineError::new(
                    "SPECIALIST_TASK_NOT_FOUND",
                    "Specialist task was not found",
                    false,
                    serde_json::json!({"task_id":task_id}),
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn expire_external_task_for_test(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<(), MachineError> {
        let changed = self
            .connection
            .execute(
                "UPDATE tasks SET created_at_ms=0 WHERE engagement_id=?1 AND task_id=?2",
                params![engagement_id.to_string(), task_id.to_string()],
            )
            .map_err(internal)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(MachineError::new(
                "SPECIALIST_TASK_NOT_FOUND",
                "Specialist task was not found",
                false,
                serde_json::json!({"task_id":task_id}),
            ))
        }
    }

    pub fn external_tasks(
        &self,
        engagement_id: Uuid,
        task_ids: &[Uuid],
    ) -> Result<Vec<ExternalTaskSnapshot>, MachineError> {
        task_ids
            .iter()
            .map(|task_id| self.external_task(engagement_id, *task_id))
            .collect()
    }

    pub fn external_active_tasks(
        &self,
        engagement_id: Uuid,
    ) -> Result<Vec<ExternalTaskSnapshot>, MachineError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT task_id FROM tasks WHERE engagement_id=?1
             AND state IN ('accepted','queued','claimed','dispatching','running')
             ORDER BY created_at_ms,task_id",
            )
            .map_err(internal)?;
        let ids = statement
            .query_map([engagement_id.to_string()], |row| row.get::<_, String>(0))
            .map_err(internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)?;
        ids.into_iter()
            .map(|id| {
                let task_id = id.parse().map_err(internal)?;
                self.external_task(engagement_id, task_id)
            })
            .collect()
    }

    pub fn collect_external_results(
        &mut self,
        engagement_id: Uuid,
        after_sequence: u64,
        limit: usize,
    ) -> Result<(Vec<ExternalTaskSnapshot>, u64), MachineError> {
        if limit == 0 || limit > 100 {
            return Err(invalid("limit", "limit must be between 1 and 100"));
        }
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let existing_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM (
                   SELECT 1 FROM delivery_receipts d JOIN tasks t ON t.task_id=d.task_id
                   WHERE t.engagement_id=?1 AND d.sequence>?2
                   LIMIT ?3
                 )",
                params![
                    engagement_id.to_string(),
                    i64::try_from(after_sequence).map_err(internal)?,
                    i64::try_from(limit).map_err(internal)?
                ],
                |row| row.get(0),
            )
            .map_err(internal)?;
        let remaining = i64::try_from(limit)
            .map_err(internal)?
            .saturating_sub(existing_count);
        let mut pending = transaction
            .prepare(
                "SELECT task_id,artifact_id FROM tasks WHERE engagement_id=?1
             AND state='completed_not_delivered' ORDER BY created_at_ms,task_id LIMIT ?2",
            )
            .map_err(internal)?;
        let rows = pending
            .query_map(params![engagement_id.to_string(), remaining], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)?;
        drop(pending);
        for (task_id, artifact_id) in rows {
            transaction.execute(
                "INSERT OR IGNORE INTO delivery_receipts(task_id,artifact_id,delivered_at_ms) VALUES(?1,?2,?3)",
                params![task_id,artifact_id,unix_time_ms()?],
            ).map_err(internal)?;
            transaction
                .execute(
                    "UPDATE tasks SET state='delivered' WHERE task_id=?1",
                    [&task_id],
                )
                .map_err(internal)?;
        }
        let mut statement = transaction.prepare(
            "SELECT d.sequence,t.task_id FROM delivery_receipts d JOIN tasks t ON t.task_id=d.task_id
             WHERE t.engagement_id=?1 AND d.sequence>?2 ORDER BY d.sequence LIMIT ?3",
        ).map_err(internal)?;
        let selected = statement
            .query_map(
                params![
                    engagement_id.to_string(),
                    i64::try_from(after_sequence).map_err(internal)?,
                    i64::try_from(limit).map_err(internal)?
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)?;
        drop(statement);
        let mut tasks = Vec::with_capacity(selected.len());
        let mut next = after_sequence;
        for (sequence, task_id) in selected {
            next = sequence.try_into().map_err(internal)?;
            tasks.push(external_task_in(
                &transaction,
                engagement_id,
                task_id.parse().map_err(internal)?,
            )?);
        }
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalDeliveryCommit,
            EngagementBarrier::AfterExternalDeliveryCommit,
        )?;
        Ok((tasks, next))
    }

    pub fn release_external_member(
        &mut self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<String, MachineError> {
        checked(idempotency_key, 256, "idempotency_key")?;
        checked_text(reason, 1024, "reason")?;
        let request = digest_value(
            &serde_json::json!({"engagement_id":engagement_id,"specialist_run_id":specialist_run_id,"reason":reason}),
        )?;
        let scope = engagement_id.to_string();
        if let Some(value) = scoped_replay(
            &self.connection,
            &scope,
            "release_external_specialist",
            idempotency_key,
            &request,
        )? {
            return Ok(value);
        }
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        if let Some(value) = scoped_replay(
            &transaction,
            &scope,
            "release_external_specialist",
            idempotency_key,
            &request,
        )? {
            return Ok(value);
        }
        let active: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE engagement_id=?1 AND specialist_run_id=?2
             AND state IN ('accepted','queued','claimed','dispatching','running'))",
                params![engagement_id.to_string(), specialist_run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if active {
            return Err(conflict("Specialist has an active task"));
        }
        let changed = transaction.execute(
            "UPDATE members SET state='released',actor_residency='terminal',updated_at_ms=?3
             WHERE engagement_id=?1 AND specialist_run_id=?2 AND state IN ('active','degraded','releasing')",
            params![engagement_id.to_string(),specialist_run_id.to_string(),unix_time_ms()?],
        ).map_err(internal)?;
        if changed == 0 {
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT state FROM members WHERE engagement_id=?1 AND specialist_run_id=?2",
                    params![engagement_id.to_string(), specialist_run_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(internal)?;
            if existing.as_deref() != Some("released") {
                return Err(specialist_not_member(specialist_run_id));
            }
        }
        let result = "released".to_owned();
        scoped_record(
            &transaction,
            &scope,
            "release_external_specialist",
            idempotency_key,
            &request,
            &result,
        )?;
        append_event(
            &transaction,
            engagement_id,
            "external_member_released",
            &specialist_run_id.to_string(),
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalMemberReleaseCommit,
            EngagementBarrier::AfterExternalMemberReleaseCommit,
        )?;
        Ok(result)
    }

    pub fn close_external_engagement(
        &mut self,
        engagement_id: Uuid,
        mode: &str,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<String, MachineError> {
        if !matches!(mode, "complete" | "abort") {
            return Err(invalid("mode", "close mode is invalid"));
        }
        checked(idempotency_key, 256, "idempotency_key")?;
        checked_text(reason, 1024, "reason")?;
        let request = digest_value(
            &serde_json::json!({"engagement_id":engagement_id,"mode":mode,"reason":reason}),
        )?;
        let scope = engagement_id.to_string();
        if let Some(value) = scoped_replay(
            &self.connection,
            &scope,
            "close_external_engagement",
            idempotency_key,
            &request,
        )? {
            return Ok(value);
        }
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        if let Some(value) = scoped_replay(
            &transaction,
            &scope,
            "close_external_engagement",
            idempotency_key,
            &request,
        )? {
            return Ok(value);
        }
        let active: bool=transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE engagement_id=?1 AND state IN ('accepted','queued','claimed','dispatching','running'))",
            [engagement_id.to_string()],|row| row.get(0),
        ).map_err(internal)?;
        if active {
            return Err(conflict("engagement has active tasks"));
        }
        let unreleased: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM members WHERE engagement_id=?1 AND state!='released')",
                [engagement_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if unreleased {
            return Err(conflict("all Specialists must be released before closing"));
        }
        let result = if mode == "complete" {
            "completed"
        } else {
            "aborted"
        }
        .to_owned();
        let changed=transaction.execute(
            "UPDATE engagements SET state=?2,revision=revision+1,updated_at_ms=?3
             WHERE engagement_id=?1 AND authority_kind='external_v1' AND state NOT IN ('completed','aborted')",
            params![engagement_id.to_string(),result,unix_time_ms()?],
        ).map_err(internal)?;
        if changed == 0 {
            return Err(conflict("engagement is already terminal"));
        }
        scoped_record(
            &transaction,
            &scope,
            "close_external_engagement",
            idempotency_key,
            &request,
            &result,
        )?;
        append_event(
            &transaction,
            engagement_id,
            "external_engagement_closed",
            &result,
        )?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeExternalCloseCommit,
            EngagementBarrier::AfterExternalCloseCommit,
        )?;
        Ok(result)
    }

    pub fn external_member_is_active(
        &self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> Result<bool, MachineError> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM members m JOIN engagements e ON e.engagement_id=m.engagement_id
             WHERE m.engagement_id=?1 AND m.specialist_run_id=?2 AND m.state='active'
             AND e.authority_kind='external_v1' AND e.state IN ('active','degraded','recovering','aborting'))",
            params![engagement_id.to_string(),specialist_run_id.to_string()],|row| row.get(0),
        ).map_err(internal)
    }

    pub fn validate_external_member_binding(
        &self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        binding: &AggregateBinding,
    ) -> Result<(), MachineError> {
        let row: Option<(String, Option<String>, String)> = self
            .connection
            .query_row(
                "SELECT hire_operation_id,role_ref,configuration_sha256 FROM members
             WHERE engagement_id=?1 AND specialist_run_id=?2",
                params![engagement_id.to_string(), specialist_run_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(internal)?;
        let (operation, role, configuration) =
            row.ok_or_else(|| specialist_not_member(specialist_run_id))?;
        if binding.operation_id.to_string() != operation
            || binding.role_reference != role
            || binding.role_snapshot_sha256
                != role.as_ref().map(|value| sha256_hex(value.as_bytes()))
            || binding.agent_configuration_sha256.as_deref() != Some(configuration.as_str())
        {
            return Err(integrity(
                "specialist Run binding does not match its member record",
            ));
        }
        Ok(())
    }

    fn transaction(&mut self) -> Result<Transaction<'_>, MachineError> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(internal)
    }

    fn idempotent_transition(
        &mut self,
        engagement_id: Uuid,
        operation: &str,
        idempotency_key: &str,
        allowed: &[&str],
        target: &str,
    ) -> Result<EngagementSnapshot, MachineError> {
        checked(idempotency_key, 256, "idempotency_key")?;
        let request = digest_value(&serde_json::json!({
            "engagement_id": engagement_id,
            "target": target
        }))?;
        if let Some(replay) =
            replay::<EngagementSnapshot>(&self.connection, operation, idempotency_key, &request)?
        {
            return Ok(replay);
        }
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        require_current(&transaction, engagement_id, allowed)?;
        transaction
            .execute(
                "UPDATE engagements SET state=?2,revision=revision+1 WHERE engagement_id=?1",
                params![engagement_id.to_string(), target],
            )
            .map_err(internal)?;
        append_event(&transaction, engagement_id, target, &request)?;
        let result = snapshot_in(&transaction, engagement_id)?;
        record(&transaction, operation, idempotency_key, &request, &result)?;
        commit_with(
            &faults,
            transaction,
            EngagementBarrier::BeforeLifecycleCommit,
            EngagementBarrier::AfterLifecycleCommit,
        )?;
        Ok(result)
    }

    fn finish_operation<T: Serialize>(
        &mut self,
        engagement_id: Uuid,
        state: &str,
        kind: &str,
        operation: &str,
        idempotency_key: &str,
        completion: OperationCompletion<'_, T>,
    ) -> Result<(), MachineError> {
        let (before, after) = match operation {
            "hire" => (
                EngagementBarrier::BeforeHireOutcomeCommit,
                EngagementBarrier::AfterHireOutcomeCommit,
            ),
            "assign" => (
                EngagementBarrier::BeforeTaskOutcomeCommit,
                EngagementBarrier::AfterTaskOutcomeCommit,
            ),
            _ => return Err(internal("unsupported engagement operation")),
        };
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE engagements SET state=?2,revision=revision+1 WHERE engagement_id=?1 AND state!='closed'",
                params![engagement_id.to_string(), state],
            )
            .map_err(internal)?;
        if changed != 1 {
            return Err(conflict("engagement is absent or terminal"));
        }
        match operation {
            "hire" => {
                transaction
                    .execute(
                        "UPDATE members SET state=?2 WHERE engagement_id=?1",
                        params![engagement_id.to_string(), state],
                    )
                    .map_err(internal)?;
            }
            "assign" => {
                let task_id = completion
                    .task_id
                    .ok_or_else(|| internal("assign completion lost its task identity"))?;
                let changed = transaction
                    .execute(
                        "UPDATE tasks SET state=?3,safe_error_code=?4 WHERE engagement_id=?1 AND task_id=?2",
                        params![
                            engagement_id.to_string(),
                            task_id.to_string(),
                            state,
                            completion.safe_error_code
                        ],
                    )
                    .map_err(internal)?;
                if changed != 1 {
                    return Err(conflict("task completion target is absent or concurrent"));
                }
            }
            _ => {}
        }
        append_event(&transaction, engagement_id, kind, state)?;
        update_response(
            &transaction,
            operation,
            idempotency_key,
            completion.response,
        )?;
        commit_with(&faults, transaction, before, after)
    }
}

fn migrate_v2_to_v3(connection: &mut Connection) -> Result<(), MachineError> {
    connection
        .execute_batch("PRAGMA foreign_keys=OFF;")
        .map_err(internal)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(internal)?;
    let observed: String = transaction
        .query_row(
            "SELECT value FROM metadata WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .map_err(internal)?;
    if observed == "2" {
        transaction
            .execute_batch(
                "DROP INDEX IF EXISTS one_active_external_task_per_member;
             ALTER TABLE engagements RENAME TO engagements_v2;
             ALTER TABLE operations RENAME TO operations_v2;
             ALTER TABLE members RENAME TO members_v2;
             ALTER TABLE tasks RENAME TO tasks_v2;
             ALTER TABLE artifacts RENAME TO artifacts_v2;
             ALTER TABLE delivery_receipts RENAME TO delivery_receipts_v2;
             ALTER TABLE events RENAME TO events_v2;

             CREATE TABLE engagements(
               engagement_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
               controller_sha256 TEXT NOT NULL, state TEXT NOT NULL,
               specialist_run_id TEXT, task_id TEXT, result_sha256 TEXT,
               result_json TEXT, revision INTEGER NOT NULL,
               bootstrap_operation_id TEXT, external_controller_ref_json TEXT,
               external_controller_ref_sha256 TEXT, label TEXT,
               controller_binding_json TEXT, created_at_ms INTEGER,
               updated_at_ms INTEGER,
               authority_kind TEXT NOT NULL DEFAULT 'legacy_one_shot'
             ) STRICT;
             INSERT INTO engagements(
               engagement_id,workspace_id,controller_sha256,state,
               specialist_run_id,task_id,result_sha256,result_json,revision
             ) SELECT engagement_id,workspace_id,controller_sha256,
               CASE state WHEN 'executing' THEN 'interrupted_unknown'
                 WHEN 'result_ready' THEN 'result_ready' ELSE state END,
               specialist_run_id,task_id,result_sha256,result_json,revision
             FROM engagements_v2;

             CREATE TABLE operations(
               scope_id TEXT NOT NULL DEFAULT 'legacy', operation TEXT NOT NULL,
               idempotency_key TEXT NOT NULL, request_sha256 TEXT NOT NULL,
               response_json TEXT NOT NULL,
               PRIMARY KEY(scope_id,operation,idempotency_key)
             ) STRICT;
             INSERT INTO operations SELECT 'legacy',operation,idempotency_key,
               request_sha256,response_json FROM operations_v2;

             CREATE TABLE members(
               specialist_run_id TEXT PRIMARY KEY, engagement_id TEXT NOT NULL,
               hire_operation_id TEXT NOT NULL UNIQUE,
               configuration_sha256 TEXT NOT NULL, state TEXT NOT NULL,
               role_ref TEXT, configuration_json TEXT, objective_sha256 TEXT,
               requested_access TEXT, actor_residency TEXT,
               created_at_ms INTEGER, updated_at_ms INTEGER,
               FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
             ) STRICT;
             INSERT INTO members(
               specialist_run_id,engagement_id,hire_operation_id,
               configuration_sha256,state
             ) SELECT specialist_run_id,engagement_id,hire_operation_id,
               configuration_sha256,state FROM members_v2;

             CREATE TABLE tasks(
               task_id TEXT PRIMARY KEY, engagement_id TEXT NOT NULL,
               specialist_run_id TEXT NOT NULL, objective_sha256 TEXT NOT NULL,
               state TEXT NOT NULL, result_sha256 TEXT,
               external_request_ref_json TEXT, request_json TEXT,
               execution_intent TEXT, deadline_seconds INTEGER, turn_id TEXT,
               artifact_id TEXT, safe_error_code TEXT,
               created_at_ms INTEGER, updated_at_ms INTEGER,
               FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id),
               FOREIGN KEY(specialist_run_id) REFERENCES members(specialist_run_id)
             ) STRICT;
             INSERT INTO tasks(
               task_id,engagement_id,specialist_run_id,objective_sha256,state,
               result_sha256
             ) SELECT task_id,engagement_id,specialist_run_id,objective_sha256,
               CASE state WHEN 'accepted' THEN 'interrupted_unknown'
                 WHEN 'result_ready' THEN 'completed_not_delivered' ELSE state END,
               result_sha256 FROM tasks_v2;

             CREATE TABLE artifacts(
               artifact_id TEXT PRIMARY KEY, engagement_id TEXT NOT NULL,
               task_id TEXT UNIQUE, specialist_run_id TEXT,
               result_sha256 TEXT NOT NULL, canonical_json TEXT NOT NULL,
               FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id),
               FOREIGN KEY(task_id) REFERENCES tasks(task_id),
               FOREIGN KEY(specialist_run_id) REFERENCES members(specialist_run_id)
             ) STRICT;
             INSERT INTO artifacts(
               artifact_id,engagement_id,result_sha256,canonical_json
             ) SELECT artifact_id,engagement_id,result_sha256,canonical_json
             FROM artifacts_v2;
             UPDATE tasks SET artifact_id=(SELECT artifact_id FROM artifacts
               WHERE artifacts.engagement_id=tasks.engagement_id LIMIT 1)
               WHERE state='completed_not_delivered';

             CREATE TABLE delivery_receipts(
               sequence INTEGER PRIMARY KEY AUTOINCREMENT,
               task_id TEXT NOT NULL UNIQUE, artifact_id TEXT NOT NULL,
               delivered_at_ms INTEGER NOT NULL,
               FOREIGN KEY(task_id) REFERENCES tasks(task_id),
               FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
             ) STRICT;
             INSERT INTO delivery_receipts SELECT sequence,task_id,artifact_id,
               delivered_at_ms FROM delivery_receipts_v2;

             CREATE TABLE events(
               sequence INTEGER PRIMARY KEY AUTOINCREMENT,
               engagement_id TEXT NOT NULL, kind TEXT NOT NULL,
               payload_sha256 TEXT NOT NULL, previous_hash TEXT NOT NULL,
               event_hash TEXT NOT NULL, created_at_ms INTEGER NOT NULL,
               FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
             ) STRICT;
             INSERT INTO events SELECT * FROM events_v2;
             CREATE UNIQUE INDEX one_active_external_task_per_member
               ON tasks(specialist_run_id)
               WHERE state IN ('accepted','queued','claimed','dispatching','running');

             DROP TABLE delivery_receipts_v2;
             DROP TABLE artifacts_v2;
             DROP TABLE tasks_v2;
             DROP TABLE members_v2;
             DROP TABLE operations_v2;
             DROP TABLE events_v2;
             DROP TABLE engagements_v2;
             UPDATE metadata SET value='3' WHERE key='schema_version';",
            )
            .map_err(internal)?;
        let violations: i64 = transaction
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(internal)?;
        if violations != 0 {
            return Err(internal(
                "orchestration v2 migration violated a foreign key",
            ));
        }
    } else if observed != SCHEMA_VERSION.to_string() {
        return Err(MachineError::new(
            "ORCHESTRATION_SCHEMA_UNSUPPORTED",
            "orchestration schema is not supported",
            false,
            serde_json::json!({"observed":observed}),
        ));
    }
    transaction.commit().map_err(internal)?;
    connection
        .execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(internal)
}

fn commit_with(
    faults: &Arc<dyn EngagementFaultInjector>,
    transaction: Transaction<'_>,
    before: EngagementBarrier,
    after: EngagementBarrier,
) -> Result<(), MachineError> {
    faults.check(before).map_err(internal)?;
    transaction.commit().map_err(internal)?;
    faults.check(after).map_err(internal)
}

fn replay<T: for<'de> Deserialize<'de>>(
    connection: &Connection,
    operation: &str,
    key: &str,
    request_sha256: &str,
) -> Result<Option<T>, MachineError> {
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT request_sha256,response_json FROM operations WHERE scope_id='legacy' AND operation=?1 AND idempotency_key=?2",
            params![operation, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((recorded, response)) = existing else {
        return Ok(None);
    };
    if recorded != request_sha256 {
        return Err(MachineError::new(
            "IDEMPOTENCY_CONFLICT",
            "idempotency key was already used with different input",
            false,
            serde_json::json!({"required_action":"use_original_input_or_new_key"}),
        ));
    }
    serde_json::from_str(&response).map(Some).map_err(internal)
}

fn scoped_replay<T: for<'de> Deserialize<'de>>(
    connection: &Connection,
    scope: &str,
    operation: &str,
    key: &str,
    request_sha256: &str,
) -> Result<Option<T>, MachineError> {
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT request_sha256,response_json FROM operations
             WHERE scope_id=?1 AND operation=?2 AND idempotency_key=?3",
            params![scope, operation, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((recorded, response)) = existing else {
        return Ok(None);
    };
    if recorded != request_sha256 {
        return Err(MachineError::new(
            "IDEMPOTENCY_CONFLICT",
            "idempotency key was already used with different input in this engagement scope",
            false,
            serde_json::json!({"required_action":"use_original_input_or_new_key"}),
        ));
    }
    serde_json::from_str(&response).map(Some).map_err(internal)
}

fn scoped_record<T: Serialize>(
    transaction: &Transaction<'_>,
    scope: &str,
    operation: &str,
    key: &str,
    request: &str,
    response: &T,
) -> Result<(), MachineError> {
    transaction.execute(
        "INSERT INTO operations(scope_id,operation,idempotency_key,request_sha256,response_json)
         VALUES(?1,?2,?3,?4,?5)",
        params![scope,operation,key,request,serde_json::to_string(response).map_err(internal)?],
    ).map_err(internal)?;
    Ok(())
}

fn scoped_update_response<T: Serialize>(
    transaction: &Transaction<'_>,
    scope: &str,
    operation: &str,
    key: &str,
    response: &T,
) -> Result<(), MachineError> {
    let changed=transaction.execute(
        "UPDATE operations SET response_json=?4 WHERE scope_id=?1 AND operation=?2 AND idempotency_key=?3",
        params![scope,operation,key,serde_json::to_string(response).map_err(internal)?],
    ).map_err(internal)?;
    if changed != 1 {
        return Err(conflict("operation receipt is missing"));
    }
    Ok(())
}

fn record<T: Serialize>(
    transaction: &Transaction<'_>,
    operation: &str,
    key: &str,
    request: &str,
    response: &T,
) -> Result<(), MachineError> {
    transaction
        .execute(
            "INSERT INTO operations(scope_id,operation,idempotency_key,request_sha256,response_json) VALUES('legacy',?1,?2,?3,?4)",
            params![
                operation,
                key,
                request,
                serde_json::to_string(response).map_err(internal)?
            ],
        )
        .map_err(internal)?;
    Ok(())
}

fn update_response<T: Serialize>(
    transaction: &Transaction<'_>,
    operation: &str,
    key: &str,
    response: &T,
) -> Result<(), MachineError> {
    let changed = transaction
        .execute(
            "UPDATE operations SET response_json=?3 WHERE scope_id='legacy' AND operation=?1 AND idempotency_key=?2",
            params![
                operation,
                key,
                serde_json::to_string(response).map_err(internal)?
            ],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Err(conflict("operation receipt is missing"));
    }
    Ok(())
}

fn append_event(
    transaction: &Transaction<'_>,
    engagement_id: Uuid,
    kind: &str,
    payload: &str,
) -> Result<(), MachineError> {
    let previous: Option<String> = transaction
        .query_row(
            "SELECT event_hash FROM events ORDER BY sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?;
    let previous = previous.unwrap_or_else(|| "0".repeat(64));
    let payload_sha256 = sha256_hex(payload.as_bytes());
    let event_hash = event_digest(&previous, engagement_id, kind, &payload_sha256);
    let created_at_ms = unix_time_ms()?;
    transaction.execute(
        "INSERT INTO events(engagement_id,kind,payload_sha256,previous_hash,event_hash,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6)",
        params![engagement_id.to_string(), kind, payload_sha256, previous, event_hash, created_at_ms],
    ).map_err(internal)?;
    Ok(())
}

fn event_digest(previous: &str, engagement_id: Uuid, kind: &str, payload_sha256: &str) -> String {
    sha256_hex(format!("{previous}\0{engagement_id}\0{kind}\0{payload_sha256}").as_bytes())
}

fn unix_time_ms() -> Result<i64, MachineError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(internal)?
            .as_millis(),
    )
    .map_err(internal)
}

fn require_current(
    connection: &Connection,
    id: Uuid,
    allowed: &[&str],
) -> Result<(), MachineError> {
    let state: Option<String> = connection
        .query_row(
            "SELECT state FROM engagements WHERE engagement_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?;
    let state = state.ok_or_else(not_found)?;
    if !allowed.contains(&state.as_str()) {
        return Err(conflict("engagement state does not allow this operation"));
    }
    Ok(())
}

fn snapshot_in(
    connection: &Connection,
    engagement_id: Uuid,
) -> Result<EngagementSnapshot, MachineError> {
    connection
        .query_row(
            "SELECT workspace_id,controller_sha256,state,specialist_run_id,task_id,result_sha256 FROM engagements WHERE engagement_id=?1",
            [engagement_id.to_string()],
            |row| {
                Ok(EngagementSnapshot {
                    engagement_id,
                    workspace_id: row.get(0)?,
                    external_controller_ref_sha256: row.get(1)?,
                    state: row.get(2)?,
                    specialist_run_id: optional_uuid(row.get::<_, Option<String>>(3)?),
                    task_id: optional_uuid(row.get::<_, Option<String>>(4)?),
                    result_sha256: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(internal)?
        .ok_or_else(not_found)
}

fn external_task_in(
    connection: &Connection,
    engagement_id: Uuid,
    task_id: Uuid,
) -> Result<ExternalTaskSnapshot, MachineError> {
    connection
        .query_row(
            "SELECT t.specialist_run_id,t.state,t.turn_id,t.artifact_id,
                    t.safe_error_code,a.canonical_json
             FROM tasks t LEFT JOIN artifacts a ON a.artifact_id=t.artifact_id
             WHERE t.engagement_id=?1 AND t.task_id=?2",
            params![engagement_id.to_string(), task_id.to_string()],
            |row| {
                let specialist: String = row.get(0)?;
                let artifact: Option<String> = row.get(3)?;
                Ok(ExternalTaskSnapshot {
                    task_id,
                    specialist_run_id: specialist
                        .parse()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    state: row.get(1)?,
                    turn_id: row.get(2)?,
                    result_artifact_ref: artifact.and_then(|value| value.parse().ok()),
                    result: row
                        .get::<_, Option<String>>(5)?
                        .map(|value| serde_json::from_str(&value))
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    safe_error_code: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(internal)?
        .ok_or_else(|| {
            MachineError::new(
                "SPECIALIST_TASK_NOT_FOUND",
                "Specialist task was not found",
                false,
                serde_json::json!({"task_id":task_id}),
            )
        })
}

fn digest_value(value: &Value) -> Result<String, MachineError> {
    Ok(sha256_hex(&canonical_json(value)?))
}

fn canonical_json(value: &Value) -> Result<Vec<u8>, MachineError> {
    let text = serde_json::to_string(value).map_err(internal)?;
    canonicalize(&parse(&text).map_err(internal)?).map_err(internal)
}

fn canonical_string(value: &Value) -> Result<String, MachineError> {
    String::from_utf8(canonical_json(value)?).map_err(internal)
}

fn access_allows(hired: &str, requested: &str) -> bool {
    matches!(
        (hired, requested),
        ("read_only", "read_only")
            | ("isolated_write", "read_only" | "isolated_write")
            | (
                "canonical_workspace_write",
                "read_only" | "canonical_workspace_write"
            )
    )
}

fn optional_uuid(value: Option<String>) -> Option<Uuid> {
    value.and_then(|value| Uuid::parse_str(&value).ok())
}

fn checked(value: &str, maximum: usize, argument: &str) -> Result<(), MachineError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(invalid(
            argument,
            "value must be nonempty, bounded, and printable",
        ));
    }
    Ok(())
}

fn checked_text(value: &str, maximum: usize, argument: &str) -> Result<(), MachineError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(invalid(argument, "value must be nonempty and bounded"));
    }
    Ok(())
}

fn digest(value: &str, argument: &str) -> Result<(), MachineError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid(
            argument,
            "value must be a lowercase SHA-256 digest",
        ));
    }
    Ok(())
}

fn invalid(argument: &str, reason: &str) -> MachineError {
    MachineError::invalid_argument(argument, reason)
}

fn conflict(reason: &str) -> MachineError {
    MachineError::new(
        "STATE_CONFLICT",
        reason,
        false,
        serde_json::json!({"required_action":"inspect_engagement"}),
    )
}

fn policy_denied(reason: &str) -> MachineError {
    MachineError::new(
        "SPECIALIST_POLICY_DENIED",
        reason,
        false,
        serde_json::json!({"required_action":"change_specialist_or_access"}),
    )
}

fn specialist_not_member(run_id: Uuid) -> MachineError {
    MachineError::new(
        "SPECIALIST_NOT_MEMBER",
        "Run is not an active member of this engagement",
        false,
        serde_json::json!({"specialist_run_id":run_id}),
    )
}

fn integrity(reason: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "external engagement integrity check failed",
        false,
        serde_json::json!({"invariant":reason}),
    )
}

fn not_found() -> MachineError {
    MachineError::new(
        "ENGAGEMENT_NOT_FOUND",
        "engagement was not found",
        false,
        serde_json::json!({"required_action":"use_existing_engagement"}),
    )
}

fn internal(reason: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable engagement authority failed",
        false,
        serde_json::json!({"reason":reason.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{binding_from_carrier, create_controller_credential};
    use crate::domain::{AggregateKind, ControllerIdentity, ControllerKind, ExecutionLane};
    use crate::run::{AggregateMemberKind, ExecutableIdentity, ProfileSnapshot};
    use crate::specialist::{REVIEWER_ROLE_REFERENCE, ReviewerRuntimeRequest};
    use crate::task_request::{
        STRUCTURED_REVIEW_OUTPUT, SpecialistTaskRequest, TaskContext, TaskCriterion,
    };
    use crate::workspace::LosslessPath;
    use std::collections::BTreeMap;

    #[test]
    fn store_error_projections_preserve_closed_contract_details() {
        let run_id = Uuid::now_v7();
        for (error, code, details) in [
            (
                conflict("invalid transition"),
                "STATE_CONFLICT",
                serde_json::json!({"required_action":"inspect_engagement"}),
            ),
            (
                policy_denied("access mismatch"),
                "SPECIALIST_POLICY_DENIED",
                serde_json::json!({"required_action":"change_specialist_or_access"}),
            ),
            (
                specialist_not_member(run_id),
                "SPECIALIST_NOT_MEMBER",
                serde_json::json!({"specialist_run_id":run_id}),
            ),
            (
                not_found(),
                "ENGAGEMENT_NOT_FOUND",
                serde_json::json!({"required_action":"use_existing_engagement"}),
            ),
        ] {
            assert_eq!(error.code, code);
            assert!(!error.retryable);
            assert_eq!(error.details, details);
            let projected = serde_json::to_value(&error).unwrap();
            assert_eq!(projected.as_object().unwrap().len(), 4);
            assert!(
                projected["message"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty())
            );
        }
    }

    struct FailAt(EngagementBarrier);

    impl EngagementFaultInjector for FailAt {
        fn check(&self, barrier: EngagementBarrier) -> Result<(), EngagementFault> {
            if barrier == self.0 {
                Err(EngagementFault(barrier))
            } else {
                Ok(())
            }
        }
    }

    fn store_with_fault(root: &Path, barrier: EngagementBarrier) -> EngagementStore {
        EngagementStore::open_with_faults(
            &root.join("orchestration.sqlite3"),
            Arc::new(FailAt(barrier)),
        )
        .unwrap()
    }

    fn store() -> (EngagementStore, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("dolgorae-engagement-{}", Uuid::now_v7()));
        let store = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        (store, root)
    }

    fn reviewer_plan(_objective: &str) -> ReviewerRuntimePlan {
        let mut profile = ProfileSnapshot {
            schema_version: 1,
            profile_name: "reviewer".to_owned(),
            canonical_codex_home: "/tmp/codex-home".to_owned(),
            normalized_argv: vec!["/usr/bin/codex".to_owned()],
            launch_cwd_policy: "profile_state_directory_v1".to_owned(),
            derived_launch_cwd: "/tmp/profile".to_owned(),
            sanitized_environment: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
                ("LC_ALL".to_owned(), "en_US.UTF-8".to_owned()),
            ]),
            enabled_features: vec!["multi_agent".to_owned()],
            disabled_features: Vec::new(),
            process_static_configuration: BTreeMap::new(),
            initial_configuration_observation: BTreeMap::new(),
            executable_identity: ExecutableIdentity {
                resolved_path: LosslessPath::Utf8("/usr/bin/codex".to_owned()),
                device: 1,
                inode: 2,
                sha256: "a".repeat(64),
            },
            codex_version: "0.153.4".to_owned(),
            app_server_schema_sha256: "b".repeat(64),
            compatibility_manifest_sha256: "c".repeat(64),
            launch_contract_sha256: String::new(),
            initial_server_key: "d".repeat(64),
        };
        profile.launch_contract_sha256 = crate::run::launch_contract_digest(&profile).unwrap();
        ReviewerRuntimePlan::resolve(
            &profile,
            ReviewerRuntimeRequest {
                runtime_profile: "reviewer".to_owned(),
                model: "gpt-5".to_owned(),
                effort: "high".to_owned(),
                required_capabilities: vec!["thread_read".to_owned()],
            },
        )
        .unwrap()
    }

    fn owner_binding() -> ControllerBinding {
        ControllerBinding {
            identity: ControllerIdentity {
                controller_id: Uuid::now_v7(),
                kind: ControllerKind::Automation,
                instance_id: "test-host".to_owned(),
                subject_id: Some("test-principal".to_owned()),
                generation: 1,
            },
            capability_sha256: "e".repeat(64),
        }
    }

    fn external_open(store: &mut EngagementStore) -> ExternalEngagementSnapshot {
        store
            .open_external_engagement(
                "workspace",
                &owner_binding(),
                &serde_json::json!({"namespace":"test","kind":"workflow","id":"faults"}),
                Some("fault matrix"),
                "open-external",
            )
            .unwrap()
    }

    fn external_ready_member(
        store: &mut EngagementStore,
        engagement_id: Uuid,
    ) -> ExternalHireReservation {
        let plan = reviewer_plan("fault matrix");
        let reservation = store
            .reserve_external_hire(
                engagement_id,
                "reviewer",
                &plan.agent_configuration,
                "fault matrix",
                "read_only",
                "hire-external",
            )
            .unwrap();
        store
            .finish_external_hire(&reservation, RuntimeOutcome::Accepted, "hire-external")
            .unwrap()
    }

    fn external_task_reservation(
        store: &mut EngagementStore,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> ExternalTaskSnapshot {
        store
            .reserve_external_task(
                engagement_id,
                specialist_run_id,
                ExternalTaskRequest {
                    request_value: &serde_json::json!({"task":"fault matrix"}),
                    objective: "exercise restart boundary",
                    external_request_ref: &serde_json::json!({"kind":"fault"}),
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "task-external",
                },
            )
            .unwrap()
    }

    #[test]
    fn external_task_request_rechecks_its_assignment_receipt_digest() {
        let (mut store, root) = store();
        let opened = external_open(&mut store);
        let member = external_ready_member(&mut store, opened.engagement_id);
        let accepted = serde_json::json!({
            "schema":"dolgorae-external-specialist-facade/v3",
            "operation":"assign_external_specialist_task",
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
        let reserved = store
            .reserve_external_task(
                opened.engagement_id,
                member.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &accepted,
                    objective: "Check completion.",
                    external_request_ref: &serde_json::json!({"kind":"review"}),
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "task-request-integrity",
                },
            )
            .unwrap();
        assert_eq!(
            store
                .external_task_request(opened.engagement_id, reserved.task_id)
                .unwrap(),
            accepted
        );
        let original: String = store
            .connection
            .query_row(
                "SELECT request_json FROM tasks WHERE task_id=?1",
                [reserved.task_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();

        let mut changed_output = accepted.clone();
        changed_output["task"]["expected_output"] = serde_json::json!("plain_text");
        let mut missing_schema = accepted.clone();
        missing_schema.as_object_mut().unwrap().remove("schema");
        for corrupted in [changed_output, missing_schema] {
            store
                .connection
                .execute(
                    "UPDATE tasks SET request_json=?2 WHERE task_id=?1",
                    params![
                        reserved.task_id.to_string(),
                        canonical_string(&corrupted).unwrap()
                    ],
                )
                .unwrap();
            let error = store
                .external_task_request(opened.engagement_id, reserved.task_id)
                .unwrap_err();
            assert_eq!(error.code, "INTERNAL_ERROR");
            assert_eq!(
                error.details["invariant"],
                "external task request digest does not match its assignment receipt"
            );
        }

        store
            .connection
            .execute(
                "UPDATE tasks SET request_json=?2 WHERE task_id=?1",
                params![reserved.task_id.to_string(), original],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO operations(
                   scope_id,operation,idempotency_key,request_sha256,response_json
                 ) SELECT scope_id,operation,'task-request-integrity-duplicate',
                          request_sha256,response_json
                   FROM operations
                  WHERE scope_id=?1
                    AND operation='assign_external_specialist_task'
                    AND idempotency_key='task-request-integrity'",
                [opened.engagement_id.to_string()],
            )
            .unwrap();
        let ambiguous = store
            .external_task_request(opened.engagement_id, reserved.task_id)
            .unwrap_err();
        assert_eq!(ambiguous.code, "INTERNAL_ERROR");
        assert_eq!(
            ambiguous.details["invariant"],
            "external task assignment receipt is missing or ambiguous"
        );
        store
            .connection
            .execute(
                "DELETE FROM operations
                  WHERE scope_id=?1
                    AND operation='assign_external_specialist_task'
                    AND idempotency_key='task-request-integrity-duplicate'",
                [opened.engagement_id.to_string()],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE tasks SET request_json=NULL WHERE task_id=?1",
                [reserved.task_id.to_string()],
            )
            .unwrap();
        let missing_request = store
            .external_task_request(opened.engagement_id, reserved.task_id)
            .unwrap_err();
        assert_eq!(missing_request.code, "INTERNAL_ERROR");
        assert_eq!(
            missing_request.details["invariant"],
            "external task request is missing"
        );
        store
            .connection
            .execute(
                "UPDATE tasks SET request_json=?2 WHERE task_id=?1",
                params![reserved.task_id.to_string(), original],
            )
            .unwrap();
        store
            .connection
            .execute(
                "DELETE FROM operations
                  WHERE scope_id=?1
                    AND operation='assign_external_specialist_task'
                    AND idempotency_key='task-request-integrity'",
                [opened.engagement_id.to_string()],
            )
            .unwrap();
        let missing_receipt = store
            .external_task_request(opened.engagement_id, reserved.task_id)
            .unwrap_err();
        assert_eq!(missing_receipt.code, "INTERNAL_ERROR");
        assert_eq!(
            missing_receipt.details["invariant"],
            "external task assignment receipt is missing or ambiguous"
        );
        let snapshot = store
            .external_task(opened.engagement_id, reserved.task_id)
            .unwrap();
        assert_eq!(snapshot.state, "accepted");
        assert_eq!(snapshot.result, None);
        assert_eq!(snapshot.result_artifact_ref, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn reopen(root: &Path) -> EngagementStore {
        EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap()
    }

    #[test]
    fn external_open_commit_faults_are_atomic_and_exactly_replayable() {
        for barrier in [
            EngagementBarrier::BeforeExternalOpenCommit,
            EngagementBarrier::AfterExternalOpenCommit,
        ] {
            let root = std::env::temp_dir()
                .join(format!("dolgorae-external-open-fault-{}", Uuid::now_v7()));
            let binding = owner_binding();
            let reference = serde_json::json!({"namespace":"test","kind":"workflow","id":"open"});
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .open_external_engagement(
                        "workspace",
                        &binding,
                        &reference,
                        None,
                        "open-fault",
                    )
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = reopen(&root);
            let opened = recovered
                .open_external_engagement("workspace", &binding, &reference, None, "open-fault")
                .unwrap();
            assert_eq!(
                recovered
                    .open_external_engagement(
                        "workspace",
                        &binding,
                        &reference,
                        None,
                        "open-fault",
                    )
                    .unwrap(),
                opened
            );
            let count: i64 = recovered
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM engagements WHERE authority_kind='external_v1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn external_hire_commit_faults_recover_each_durable_boundary() {
        for barrier in [
            EngagementBarrier::BeforeExternalHireReservationCommit,
            EngagementBarrier::AfterExternalHireReservationCommit,
        ] {
            let (mut initial, root) = store();
            let opened = external_open(&mut initial);
            let plan = reviewer_plan("fault matrix");
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .reserve_external_hire(
                        opened.engagement_id,
                        "reviewer",
                        &plan.agent_configuration,
                        "fault matrix",
                        "read_only",
                        "hire-fault",
                    )
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = reopen(&root);
            let reservation = recovered
                .reserve_external_hire(
                    opened.engagement_id,
                    "reviewer",
                    &plan.agent_configuration,
                    "fault matrix",
                    "read_only",
                    "hire-fault",
                )
                .unwrap();
            assert_eq!(
                recovered
                    .reserve_external_hire(
                        opened.engagement_id,
                        "reviewer",
                        &plan.agent_configuration,
                        "fault matrix",
                        "read_only",
                        "hire-fault",
                    )
                    .unwrap(),
                reservation
            );
            assert_eq!(
                recovered
                    .external_snapshot(opened.engagement_id)
                    .unwrap()
                    .specialists
                    .len(),
                1
            );
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }

        for barrier in [
            EngagementBarrier::BeforeExternalHireOutcomeCommit,
            EngagementBarrier::AfterExternalHireOutcomeCommit,
        ] {
            let (mut initial, root) = store();
            let opened = external_open(&mut initial);
            let plan = reviewer_plan("fault matrix");
            let reservation = initial
                .reserve_external_hire(
                    opened.engagement_id,
                    "reviewer",
                    &plan.agent_configuration,
                    "fault matrix",
                    "read_only",
                    "hire-fault",
                )
                .unwrap();
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .finish_external_hire(&reservation, RuntimeOutcome::Accepted, "hire-fault",)
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = reopen(&root);
            assert_eq!(
                recovered
                    .finish_external_hire(&reservation, RuntimeOutcome::Accepted, "hire-fault",)
                    .unwrap()
                    .state,
                "ready"
            );
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }

        for barrier in [
            EngagementBarrier::BeforeExternalMemberResidencyCommit,
            EngagementBarrier::AfterExternalMemberResidencyCommit,
        ] {
            let (mut initial, root) = store();
            let opened = external_open(&mut initial);
            let member = external_ready_member(&mut initial, opened.engagement_id);
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .mark_external_member_resident(opened.engagement_id, member.specialist_run_id)
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = reopen(&root);
            recovered
                .mark_external_member_resident(opened.engagement_id, member.specialist_run_id)
                .unwrap();
            assert_eq!(
                recovered
                    .external_member_actor_residency(
                        opened.engagement_id,
                        member.specialist_run_id,
                    )
                    .unwrap(),
                "resident"
            );
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn external_task_commit_faults_recover_each_durable_boundary() {
        for barrier in [
            EngagementBarrier::BeforeExternalTaskReservationCommit,
            EngagementBarrier::AfterExternalTaskReservationCommit,
        ] {
            let (mut initial, root) = store();
            let opened = external_open(&mut initial);
            let member = external_ready_member(&mut initial, opened.engagement_id);
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .reserve_external_task(
                        opened.engagement_id,
                        member.specialist_run_id,
                        ExternalTaskRequest {
                            request_value: &serde_json::json!({"task":"fault"}),
                            objective: "fault",
                            external_request_ref: &serde_json::json!({"kind":"fault"}),
                            execution_intent: "read_only",
                            deadline_seconds: 60,
                            idempotency_key: "task-fault",
                        },
                    )
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = reopen(&root);
            let task = recovered
                .reserve_external_task(
                    opened.engagement_id,
                    member.specialist_run_id,
                    ExternalTaskRequest {
                        request_value: &serde_json::json!({"task":"fault"}),
                        objective: "fault",
                        external_request_ref: &serde_json::json!({"kind":"fault"}),
                        execution_intent: "read_only",
                        deadline_seconds: 60,
                        idempotency_key: "task-fault",
                    },
                )
                .unwrap();
            assert_eq!(task.state, "accepted");
            assert_eq!(
                recovered
                    .external_active_tasks(opened.engagement_id)
                    .unwrap()
                    .len(),
                1
            );
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }

        for barrier in [
            EngagementBarrier::BeforeExternalTaskDispatchCommit,
            EngagementBarrier::AfterExternalTaskDispatchCommit,
            EngagementBarrier::BeforeExternalTaskRunningCommit,
            EngagementBarrier::AfterExternalTaskRunningCommit,
            EngagementBarrier::BeforeExternalTaskPendingCommit,
            EngagementBarrier::AfterExternalTaskPendingCommit,
            EngagementBarrier::BeforeExternalTaskTerminalCommit,
            EngagementBarrier::AfterExternalTaskTerminalCommit,
            EngagementBarrier::BeforeExternalTaskCancelCommit,
            EngagementBarrier::AfterExternalTaskCancelCommit,
        ] {
            let (mut initial, root) = store();
            let opened = external_open(&mut initial);
            let member = external_ready_member(&mut initial, opened.engagement_id);
            let task = external_task_reservation(
                &mut initial,
                opened.engagement_id,
                member.specialist_run_id,
            );
            let dispatch_boundary = matches!(
                barrier,
                EngagementBarrier::BeforeExternalTaskDispatchCommit
                    | EngagementBarrier::AfterExternalTaskDispatchCommit
            );
            let running_boundary = matches!(
                barrier,
                EngagementBarrier::BeforeExternalTaskRunningCommit
                    | EngagementBarrier::AfterExternalTaskRunningCommit
            );
            let terminal_boundary = matches!(
                barrier,
                EngagementBarrier::BeforeExternalTaskTerminalCommit
                    | EngagementBarrier::AfterExternalTaskTerminalCommit
            );
            let pending_boundary = matches!(
                barrier,
                EngagementBarrier::BeforeExternalTaskPendingCommit
                    | EngagementBarrier::AfterExternalTaskPendingCommit
            );
            if !dispatch_boundary
                && !matches!(
                    barrier,
                    EngagementBarrier::BeforeExternalTaskCancelCommit
                        | EngagementBarrier::AfterExternalTaskCancelCommit
                )
            {
                initial
                    .mark_external_task_dispatching(
                        opened.engagement_id,
                        task.task_id,
                        "task-external",
                    )
                    .unwrap();
            }
            if terminal_boundary || pending_boundary {
                initial
                    .mark_external_task_running(
                        opened.engagement_id,
                        task.task_id,
                        "turn-fault",
                        "task-external",
                    )
                    .unwrap();
            }
            drop(initial);
            let structured_report = serde_json::json!({
                "summary":"fault result",
                "findings":[],
                "criterion_assessments":[{
                    "criterion_id":"C-1",
                    "status":"met",
                    "explanation":"durable evidence",
                    "evidence":[{
                        "basis":"candidate",
                        "description":"captured source",
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
            let mut faulted = store_with_fault(&root, barrier);
            let error = if dispatch_boundary {
                faulted
                    .mark_external_task_dispatching(
                        opened.engagement_id,
                        task.task_id,
                        "task-external",
                    )
                    .unwrap_err()
            } else if running_boundary {
                faulted
                    .mark_external_task_running(
                        opened.engagement_id,
                        task.task_id,
                        "turn-fault",
                        "task-external",
                    )
                    .unwrap_err()
            } else if pending_boundary {
                faulted
                    .mark_external_task_result_pending(
                        opened.engagement_id,
                        task.task_id,
                        "OUTCOME_UNKNOWN",
                    )
                    .unwrap_err()
            } else if terminal_boundary {
                faulted
                    .finish_external_task(
                        opened.engagement_id,
                        task.task_id,
                        Some(&structured_report),
                        "completed_not_delivered",
                        None,
                    )
                    .unwrap_err()
            } else {
                faulted
                    .cancel_external_task(
                        opened.engagement_id,
                        task.task_id,
                        "cancelled",
                        "fault",
                        "cancel-fault",
                    )
                    .unwrap_err()
            };
            assert_eq!(error.code, "INTERNAL_ERROR");
            drop(faulted);
            let mut recovered = reopen(&root);
            let recovered_task = if dispatch_boundary {
                recovered
                    .mark_external_task_dispatching(
                        opened.engagement_id,
                        task.task_id,
                        "task-external",
                    )
                    .unwrap()
            } else if running_boundary {
                recovered
                    .mark_external_task_running(
                        opened.engagement_id,
                        task.task_id,
                        "turn-fault",
                        "task-external",
                    )
                    .unwrap()
            } else if pending_boundary {
                recovered
                    .mark_external_task_result_pending(
                        opened.engagement_id,
                        task.task_id,
                        "OUTCOME_UNKNOWN",
                    )
                    .unwrap()
            } else if terminal_boundary {
                recovered
                    .finish_external_task(
                        opened.engagement_id,
                        task.task_id,
                        Some(&structured_report),
                        "completed_not_delivered",
                        None,
                    )
                    .unwrap()
            } else {
                recovered
                    .cancel_external_task(
                        opened.engagement_id,
                        task.task_id,
                        "cancelled",
                        "fault",
                        "cancel-fault",
                    )
                    .unwrap()
            };
            assert_eq!(
                recovered_task.state,
                if dispatch_boundary {
                    "dispatching"
                } else if running_boundary || pending_boundary {
                    "running"
                } else if terminal_boundary {
                    "completed_not_delivered"
                } else {
                    "cancelled"
                }
            );
            if terminal_boundary {
                let artifacts: i64 = recovered
                    .connection
                    .query_row(
                        "SELECT COUNT(*) FROM artifacts WHERE task_id=?1",
                        [task.task_id.to_string()],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(artifacts, 1);
                assert_eq!(recovered_task.result, Some(structured_report));
            }
            if pending_boundary {
                assert_eq!(
                    recovered_task.safe_error_code.as_deref(),
                    Some("OUTCOME_UNKNOWN")
                );
            }
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn external_delivery_and_close_commit_faults_are_exactly_replayable() {
        for barrier in [
            EngagementBarrier::BeforeExternalDeliveryCommit,
            EngagementBarrier::AfterExternalDeliveryCommit,
        ] {
            let (mut initial, root) = store();
            let opened = external_open(&mut initial);
            let member = external_ready_member(&mut initial, opened.engagement_id);
            let task = external_task_reservation(
                &mut initial,
                opened.engagement_id,
                member.specialist_run_id,
            );
            let structured_report = serde_json::json!({
                "summary":"deliver",
                "findings":[],
                "criterion_assessments":[{
                    "criterion_id":"C-1",
                    "status":"unverified",
                    "explanation":"runtime evidence is unavailable",
                    "evidence":[{
                        "basis":"unavailable",
                        "description":"no live runtime",
                        "path":null,
                        "line_start":null,
                        "line_end":null,
                        "context_id":null
                    }],
                    "remaining_gap":"run an authorized live check"
                }],
                "evidence_limits":["no live runtime"],
                "overall_assessment":"insufficient_evidence"
            });
            initial
                .finish_external_task(
                    opened.engagement_id,
                    task.task_id,
                    Some(&structured_report),
                    "completed_not_delivered",
                    None,
                )
                .unwrap();
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .collect_external_results(opened.engagement_id, 0, 8)
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = reopen(&root);
            let (tasks, first_cursor) = recovered
                .collect_external_results(opened.engagement_id, 0, 8)
                .unwrap();
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].state, "delivered");
            assert_eq!(tasks[0].result.as_ref(), Some(&structured_report));
            let (redelivered, replay_cursor) = recovered
                .collect_external_results(opened.engagement_id, 0, 8)
                .unwrap();
            assert_eq!(redelivered[0].result.as_ref(), Some(&structured_report));
            assert_eq!(replay_cursor, first_cursor);
            let receipts: i64 = recovered
                .connection
                .query_row("SELECT COUNT(*) FROM delivery_receipts", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(receipts, 1);
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }

        for barrier in [
            EngagementBarrier::BeforeExternalMemberReleaseCommit,
            EngagementBarrier::AfterExternalMemberReleaseCommit,
        ] {
            let (mut initial, root) = store();
            let opened = external_open(&mut initial);
            let member = external_ready_member(&mut initial, opened.engagement_id);
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .release_external_member(
                        opened.engagement_id,
                        member.specialist_run_id,
                        "fault",
                        "release-fault",
                    )
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = reopen(&root);
            assert_eq!(
                recovered
                    .release_external_member(
                        opened.engagement_id,
                        member.specialist_run_id,
                        "fault",
                        "release-fault",
                    )
                    .unwrap(),
                "released"
            );
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }

        for barrier in [
            EngagementBarrier::BeforeExternalCloseCommit,
            EngagementBarrier::AfterExternalCloseCommit,
        ] {
            let (mut initial, root) = store();
            let opened = external_open(&mut initial);
            let member = external_ready_member(&mut initial, opened.engagement_id);
            initial
                .release_external_member(
                    opened.engagement_id,
                    member.specialist_run_id,
                    "complete",
                    "release-complete",
                )
                .unwrap();
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .close_external_engagement(
                        opened.engagement_id,
                        "complete",
                        "fault",
                        "close-fault",
                    )
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = reopen(&root);
            assert_eq!(
                recovered
                    .close_external_engagement(
                        opened.engagement_id,
                        "complete",
                        "fault",
                        "close-fault",
                    )
                    .unwrap(),
                "completed"
            );
            drop(recovered);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn external_owner_integrity_corruption_fails_closed() {
        let (mut store, root) = store();
        let credential_path = root.join("owner.json");
        create_controller_credential(
            &credential_path,
            ControllerKind::Automation,
            "integrity-owner".to_owned(),
            None,
            None,
        )
        .unwrap();
        let carrier = CredentialCarrier::open_path(&credential_path).unwrap();
        let binding = binding_from_carrier(&carrier, 1).unwrap();
        let opened = store
            .open_external_engagement(
                "workspace",
                &binding,
                &serde_json::json!({"namespace":"test","kind":"workflow","id":"integrity"}),
                None,
                "open-integrity",
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE engagements SET controller_binding_json='{}',controller_sha256=?2
                 WHERE engagement_id=?1",
                params![opened.engagement_id.to_string(), sha256_hex(b"{}")],
            )
            .unwrap();

        let error = store
            .authorize_external_owner(
                "workspace",
                opened.engagement_id,
                "get_external_engagement",
                &carrier,
            )
            .unwrap_err();
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert_eq!(error.message, "external engagement integrity check failed");
        assert_eq!(
            store.external_snapshot(opened.engagement_id).unwrap().state,
            "active"
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reusable_engagement_supports_multiple_members_and_sequential_tasks() {
        let (mut store, root) = store();
        let binding = owner_binding();
        let opened = store
            .open_external_engagement(
                "workspace",
                &binding,
                &serde_json::json!({"namespace":"test","kind":"workflow","id":"one"}),
                Some("test engagement"),
                "open",
            )
            .unwrap();
        assert_eq!(
            store
                .open_external_engagement(
                    "workspace",
                    &binding,
                    &serde_json::json!({"namespace":"test","kind":"workflow","id":"one"}),
                    Some("test engagement"),
                    "open",
                )
                .unwrap()
                .engagement_id,
            opened.engagement_id,
        );
        let plan = reviewer_plan("review");
        let mut members = Vec::new();
        for number in 0..2 {
            let reserved = store
                .reserve_external_hire(
                    opened.engagement_id,
                    &format!("reviewer-{number}"),
                    &plan.agent_configuration,
                    "review",
                    "read_only",
                    &format!("hire-{number}"),
                )
                .unwrap();
            let ready = store
                .finish_external_hire(
                    &reserved,
                    RuntimeOutcome::Accepted,
                    &format!("hire-{number}"),
                )
                .unwrap();
            assert_eq!(ready.state, "ready");
            members.push(ready.specialist_run_id);
        }
        let snapshot = store.external_snapshot(opened.engagement_id).unwrap();
        assert_eq!(snapshot.specialists.len(), 2);
        assert!(
            snapshot
                .specialists
                .iter()
                .all(|member| member.actor_residency == "unstarted")
        );
        store
            .mark_external_member_resident(opened.engagement_id, members[0])
            .unwrap();

        for number in 0..2 {
            let request = serde_json::json!({"task":number});
            let external_ref =
                serde_json::json!({"namespace":"test","kind":"request","id":format!("{number}")});
            let task = store
                .reserve_external_task(
                    opened.engagement_id,
                    members[0],
                    ExternalTaskRequest {
                        request_value: &request,
                        objective: "inspect",
                        external_request_ref: &external_ref,
                        execution_intent: "read_only",
                        deadline_seconds: 60,
                        idempotency_key: &format!("assign-{number}"),
                    },
                )
                .unwrap();
            store
                .mark_external_task_dispatching(
                    opened.engagement_id,
                    task.task_id,
                    &format!("assign-{number}"),
                )
                .unwrap();
            let running = store
                .mark_external_task_running(
                    opened.engagement_id,
                    task.task_id,
                    &format!("turn-{number}"),
                    &format!("assign-{number}"),
                )
                .unwrap();
            assert_eq!(running.state, "running");
            let completed = store
                .finish_external_task(
                    opened.engagement_id,
                    task.task_id,
                    Some(&serde_json::json!({"summary":format!("result-{number}")})),
                    "completed_not_delivered",
                    None,
                )
                .unwrap();
            assert!(completed.result_artifact_ref.is_some());
            assert_eq!(
                completed.result,
                Some(serde_json::json!({"summary":format!("result-{number}")}))
            );
        }
        let expiring = store
            .reserve_external_task(
                opened.engagement_id,
                members[0],
                ExternalTaskRequest {
                    request_value: &serde_json::json!({"task":"deadline"}),
                    objective: "inspect deadline",
                    external_request_ref: &serde_json::json!({"kind":"deadline"}),
                    execution_intent: "read_only",
                    deadline_seconds: 1,
                    idempotency_key: "assign-deadline",
                },
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE tasks SET created_at_ms=0 WHERE task_id=?1",
                [expiring.task_id.to_string()],
            )
            .unwrap();
        assert!(
            store
                .external_task_deadline_expired(opened.engagement_id, expiring.task_id)
                .unwrap()
        );
        store
            .finish_external_task(
                opened.engagement_id,
                expiring.task_id,
                None,
                "expired",
                Some("OPERATION_TIMEOUT"),
            )
            .unwrap();
        drop(store);
        let mut store = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        assert_eq!(
            store
                .external_snapshot(opened.engagement_id)
                .unwrap()
                .specialists
                .len(),
            2
        );
        let (first_page, first_cursor) = store
            .collect_external_results(opened.engagement_id, 0, 1)
            .unwrap();
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].state, "delivered");
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM tasks WHERE engagement_id=?1 AND state='completed_not_delivered'",
                    [opened.engagement_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        let (second_page, cursor) = store
            .collect_external_results(opened.engagement_id, first_cursor, 1)
            .unwrap();
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].state, "delivered");
        let (redelivered, same_cursor) = store
            .collect_external_results(opened.engagement_id, 0, 100)
            .unwrap();
        assert_eq!(redelivered.len(), 2);
        assert!(redelivered.iter().all(|task| task.result.is_some()));
        assert_eq!(same_cursor, cursor);
        let (none, _) = store
            .collect_external_results(opened.engagement_id, cursor, 100)
            .unwrap();
        assert!(none.is_empty());

        for (number, member) in members.into_iter().enumerate() {
            assert_eq!(
                store
                    .release_external_member(
                        opened.engagement_id,
                        member,
                        "task complete",
                        &format!("release-{number}")
                    )
                    .unwrap(),
                "released"
            );
            assert_eq!(
                store
                    .release_external_member(
                        opened.engagement_id,
                        member,
                        "changed reason",
                        &format!("release-{number}")
                    )
                    .unwrap_err()
                    .code,
                "IDEMPOTENCY_CONFLICT"
            );
        }
        assert_eq!(
            store
                .close_external_engagement(
                    opened.engagement_id,
                    "complete",
                    "all members released",
                    "close",
                )
                .unwrap(),
            "completed"
        );
        assert_eq!(
            store
                .close_external_engagement(
                    opened.engagement_id,
                    "complete",
                    "changed reason",
                    "close",
                )
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reusable_engagement_enforces_scoped_idempotency_and_one_active_task() {
        let (mut store, root) = store();
        let binding = owner_binding();
        let open = |store: &mut EngagementStore, id: &str| {
            store
                .open_external_engagement(
                    "workspace",
                    &binding,
                    &serde_json::json!({"namespace":"test","kind":"workflow","id":id}),
                    None,
                    id,
                )
                .unwrap()
        };
        let first = open(&mut store, "first");
        let second = open(&mut store, "second");
        let plan = reviewer_plan("review");
        let mut member_ids = Vec::new();
        for engagement in [first.engagement_id, second.engagement_id] {
            let member = store
                .reserve_external_hire(
                    engagement,
                    "reviewer",
                    &plan.agent_configuration,
                    "review",
                    "read_only",
                    "same-key",
                )
                .unwrap();
            store
                .finish_external_hire(&member, RuntimeOutcome::Accepted, "same-key")
                .unwrap();
            member_ids.push(member.specialist_run_id);
        }
        let request = serde_json::json!({"same":true});
        let first_ref = serde_json::json!({"namespace":"test","kind":"request","id":"one"});
        let task = store
            .reserve_external_task(
                first.engagement_id,
                member_ids[0],
                ExternalTaskRequest {
                    request_value: &request,
                    objective: "one",
                    external_request_ref: &first_ref,
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "task",
                },
            )
            .unwrap();
        let conflicting_request = serde_json::json!({"same":false});
        let conflicting_ref = serde_json::json!({"namespace":"test","kind":"request","id":"two"});
        let conflict = store
            .reserve_external_task(
                first.engagement_id,
                member_ids[0],
                ExternalTaskRequest {
                    request_value: &conflicting_request,
                    objective: "two",
                    external_request_ref: &conflicting_ref,
                    execution_intent: "read_only",
                    deadline_seconds: 60,
                    idempotency_key: "other-task",
                },
            )
            .unwrap_err();
        assert_eq!(conflict.code, "STATE_CONFLICT");
        assert_eq!(
            store
                .reserve_external_task(
                    first.engagement_id,
                    member_ids[0],
                    ExternalTaskRequest {
                        request_value: &request,
                        objective: "one",
                        external_request_ref: &first_ref,
                        execution_intent: "read_only",
                        deadline_seconds: 60,
                        idempotency_key: "task",
                    },
                )
                .unwrap()
                .task_id,
            task.task_id
        );
        assert_eq!(
            store
                .reserve_external_hire(
                    first.engagement_id,
                    "different",
                    &plan.agent_configuration,
                    "review",
                    "read_only",
                    "same-key",
                )
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        let cancelled = store
            .cancel_external_task(
                first.engagement_id,
                task.task_id,
                "interrupted_unknown",
                "stop requested",
                "cancel-task",
            )
            .unwrap();
        assert_eq!(cancelled.state, "interrupted_unknown");
        assert_eq!(
            store
                .cancel_external_task(
                    first.engagement_id,
                    task.task_id,
                    "interrupted_unknown",
                    "different reason",
                    "cancel-task",
                )
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_exact_open_replay_publishes_one_engagement() {
        let (initial, root) = store();
        drop(initial);
        let database = root.join("orchestration.sqlite3");
        let binding = owner_binding();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let database = database.clone();
            let binding = binding.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut store = EngagementStore::open(&database).unwrap();
                barrier.wait();
                store
                    .open_external_engagement(
                        "workspace",
                        &binding,
                        &serde_json::json!({"namespace":"test","kind":"workflow","id":"same"}),
                        None,
                        "concurrent-open",
                    )
                    .unwrap()
            }));
        }
        barrier.wait();
        let first = handles.remove(0).join().unwrap();
        let second = handles.remove(0).join().unwrap();
        assert_eq!(first.engagement_id, second.engagement_id);
        let mut reopened = EngagementStore::open(&database).unwrap();
        let plan = reviewer_plan("concurrent review");
        let member = reopened
            .reserve_external_hire(
                first.engagement_id,
                "reviewer",
                &plan.agent_configuration,
                "concurrent review",
                "read_only",
                "concurrent-hire",
            )
            .unwrap();
        reopened
            .finish_external_hire(&member, RuntimeOutcome::Accepted, "concurrent-hire")
            .unwrap();
        drop(reopened);

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let database = database.clone();
            let barrier = Arc::clone(&barrier);
            let engagement_id = first.engagement_id;
            let specialist_run_id = member.specialist_run_id;
            handles.push(std::thread::spawn(move || {
                let mut store = EngagementStore::open(&database).unwrap();
                let request = serde_json::json!({"task":"same"});
                let external_ref = serde_json::json!({"kind":"request","id":"same"});
                barrier.wait();
                store
                    .reserve_external_task(
                        engagement_id,
                        specialist_run_id,
                        ExternalTaskRequest {
                            request_value: &request,
                            objective: "same task",
                            external_request_ref: &external_ref,
                            execution_intent: "read_only",
                            deadline_seconds: 60,
                            idempotency_key: "concurrent-task",
                        },
                    )
                    .unwrap()
            }));
        }
        barrier.wait();
        let first_task = handles.remove(0).join().unwrap();
        let second_task = handles.remove(0).join().unwrap();
        assert_eq!(first_task.task_id, second_task.task_id);

        let reopened = EngagementStore::open(&database).unwrap();
        let counts: (i64, i64, i64) = reopened
            .connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM engagements),
                        (SELECT COUNT(*) FROM operations
                         WHERE operation='open_external_engagement'),
                        (SELECT COUNT(*) FROM tasks)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1, 1));
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_hire_recovery_and_duplicate_settlement_are_durable() {
        let (mut store, root) = store();
        let opened = store
            .open_external_engagement(
                "workspace",
                &owner_binding(),
                &serde_json::json!({"namespace":"test","kind":"workflow","id":"hire-recovery"}),
                None,
                "open-hire-recovery",
            )
            .unwrap();
        let plan = reviewer_plan("recover hire");
        let settled = store
            .reserve_external_hire(
                opened.engagement_id,
                "settled",
                &plan.agent_configuration,
                "settle once",
                "read_only",
                "settled-hire",
            )
            .unwrap();
        assert_eq!(
            store
                .finish_external_hire(&settled, RuntimeOutcome::Accepted, "settled-hire")
                .unwrap()
                .state,
            "ready"
        );
        assert_eq!(
            store
                .finish_external_hire(&settled, RuntimeOutcome::Rejected, "settled-hire")
                .unwrap()
                .state,
            "ready"
        );
        assert_eq!(
            store
                .finish_external_hire(&settled, RuntimeOutcome::Accepted, "wrong-settlement-key")
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        assert_eq!(
            store.external_snapshot(opened.engagement_id).unwrap().state,
            "active"
        );

        let stale = store
            .reserve_external_hire(
                opened.engagement_id,
                "stale",
                &plan.agent_configuration,
                "recover stale",
                "read_only",
                "stale-hire",
            )
            .unwrap();
        assert!(
            store
                .stale_external_hires(opened.engagement_id, Duration::from_secs(300))
                .unwrap()
                .is_empty()
        );
        store
            .connection
            .execute(
                "UPDATE members SET updated_at_ms=0 WHERE specialist_run_id=?1",
                [stale.specialist_run_id.to_string()],
            )
            .unwrap();
        let recoverable = store
            .stale_external_hires(opened.engagement_id, Duration::from_secs(300))
            .unwrap();
        assert_eq!(recoverable.len(), 1);
        assert_eq!(recoverable[0].0.specialist_run_id, stale.specialist_run_id);
        assert_eq!(recoverable[0].1, "stale-hire");
        store
            .finish_external_hire(&stale, RuntimeOutcome::Rejected, "stale-hire")
            .unwrap();
        assert_eq!(
            store
                .external_member_actor_residency(opened.engagement_id, stale.specialist_run_id)
                .unwrap(),
            "unavailable"
        );
        assert_eq!(
            store
                .release_external_member(
                    opened.engagement_id,
                    stale.specialist_run_id,
                    "recovered stale reservation",
                    "release-stale-hire",
                )
                .unwrap(),
            "released"
        );

        let recovering = store
            .reserve_external_hire(
                opened.engagement_id,
                "recovering",
                &plan.agent_configuration,
                "preserve uncertain publication",
                "read_only",
                "recovering-hire",
            )
            .unwrap();
        assert_eq!(
            store
                .finish_external_hire(&recovering, RuntimeOutcome::Unknown, "recovering-hire",)
                .unwrap()
                .state,
            "recovery_required"
        );
        assert_eq!(
            store
                .external_member_actor_residency(
                    opened.engagement_id,
                    recovering.specialist_run_id,
                )
                .unwrap(),
            "recovering"
        );
        assert_eq!(
            store.external_snapshot(opened.engagement_id).unwrap().state,
            "degraded"
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_guards_preserve_fresh_work_and_writer_eligibility() {
        let (mut store, root) = store();
        let opened = store
            .open_external_engagement(
                "workspace",
                &owner_binding(),
                &serde_json::json!({"namespace":"test","kind":"workflow","id":"guards"}),
                None,
                "open-guards",
            )
            .unwrap();
        let plan = reviewer_plan("guard task transitions");
        let canonical = store
            .reserve_external_hire(
                opened.engagement_id,
                "writer",
                &plan.agent_configuration,
                "write",
                "canonical_workspace_write",
                "hire-writer",
            )
            .unwrap();
        store
            .finish_external_hire(&canonical, RuntimeOutcome::Accepted, "hire-writer")
            .unwrap();
        assert_eq!(
            store
                .external_canonical_members_without_active_tasks(opened.engagement_id)
                .unwrap(),
            vec![canonical.specialist_run_id]
        );
        let request = serde_json::json!({"task":"write"});
        let external_ref = serde_json::json!({"kind":"request","id":"write"});
        assert_eq!(
            store
                .reserve_external_task(
                    opened.engagement_id,
                    canonical.specialist_run_id,
                    ExternalTaskRequest {
                        request_value: &request,
                        objective: "wrong workspace policy",
                        external_request_ref: &external_ref,
                        execution_intent: "isolated_write",
                        deadline_seconds: 60,
                        idempotency_key: "isolated-on-canonical",
                    },
                )
                .unwrap_err()
                .code,
            "SPECIALIST_POLICY_DENIED"
        );
        assert_eq!(
            store
                .reserve_external_task(
                    opened.engagement_id,
                    canonical.specialist_run_id,
                    ExternalTaskRequest {
                        request_value: &request,
                        objective: "write",
                        external_request_ref: &external_ref,
                        execution_intent: "canonical_workspace_write",
                        deadline_seconds: 0,
                        idempotency_key: "invalid-deadline",
                    },
                )
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
        let task = store
            .reserve_external_task(
                opened.engagement_id,
                canonical.specialist_run_id,
                ExternalTaskRequest {
                    request_value: &request,
                    objective: "write",
                    external_request_ref: &external_ref,
                    execution_intent: "canonical_workspace_write",
                    deadline_seconds: 60,
                    idempotency_key: "valid-task",
                },
            )
            .unwrap();
        assert!(
            !store
                .external_task_deadline_expired(opened.engagement_id, task.task_id)
                .unwrap()
        );
        assert!(
            store
                .external_canonical_members_without_active_tasks(opened.engagement_id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .mark_external_task_running(
                    opened.engagement_id,
                    task.task_id,
                    "turn-before-dispatch",
                    "valid-task",
                )
                .unwrap_err()
                .code,
            "STATE_CONFLICT"
        );
        store
            .finish_external_task(
                opened.engagement_id,
                task.task_id,
                None,
                "failed",
                Some("TEST_TERMINAL"),
            )
            .unwrap();
        assert_eq!(
            store
                .external_canonical_members_without_active_tasks(opened.engagement_id)
                .unwrap(),
            vec![canonical.specialist_run_id]
        );

        let reader = store
            .reserve_external_hire(
                opened.engagement_id,
                "reader",
                &plan.agent_configuration,
                "read",
                "read_only",
                "hire-reader",
            )
            .unwrap();
        store
            .finish_external_hire(&reader, RuntimeOutcome::Accepted, "hire-reader")
            .unwrap();
        assert_eq!(
            store
                .reserve_external_task(
                    opened.engagement_id,
                    reader.specialist_run_id,
                    ExternalTaskRequest {
                        request_value: &request,
                        objective: "escalate",
                        external_request_ref: &external_ref,
                        execution_intent: "isolated_write",
                        deadline_seconds: 60,
                        idempotency_key: "excess-access",
                    },
                )
                .unwrap_err()
                .code,
            "SPECIALIST_POLICY_DENIED"
        );
        store
            .release_external_member(
                opened.engagement_id,
                reader.specialist_run_id,
                "done",
                "release-reader",
            )
            .unwrap();
        let isolated = store
            .reserve_external_hire(
                opened.engagement_id,
                "isolated writer",
                &plan.agent_configuration,
                "isolated",
                "isolated_write",
                "hire-isolated-writer",
            )
            .unwrap();
        store
            .finish_external_hire(&isolated, RuntimeOutcome::Accepted, "hire-isolated-writer")
            .unwrap();
        assert_eq!(
            store
                .reserve_external_task(
                    opened.engagement_id,
                    isolated.specialist_run_id,
                    ExternalTaskRequest {
                        request_value: &request,
                        objective: "wrong canonical policy",
                        external_request_ref: &external_ref,
                        execution_intent: "canonical_workspace_write",
                        deadline_seconds: 60,
                        idempotency_key: "canonical-on-isolated",
                    },
                )
                .unwrap_err()
                .code,
            "SPECIALIST_POLICY_DENIED"
        );
        assert_eq!(
            store
                .reserve_external_task(
                    opened.engagement_id,
                    reader.specialist_run_id,
                    ExternalTaskRequest {
                        request_value: &request,
                        objective: "after release",
                        external_request_ref: &external_ref,
                        execution_intent: "read_only",
                        deadline_seconds: 60,
                        idempotency_key: "released-member-task",
                    },
                )
                .unwrap_err()
                .code,
            "STATE_CONFLICT"
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fresh_schema_keeps_artifact_foreign_keys_and_event_digest_detects_tampering() {
        let (mut store, root) = store();
        let mut statement = store
            .connection
            .prepare("PRAGMA foreign_key_list(artifacts)")
            .unwrap();
        let mut targets = statement
            .query_map([], |row| row.get::<_, String>(2))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        targets.sort();
        assert_eq!(targets, ["engagements", "members", "tasks"]);
        drop(statement);

        let binding = owner_binding();
        let opened = store
            .open_external_engagement(
                "workspace",
                &binding,
                &serde_json::json!({"namespace":"test","kind":"workflow","id":"tamper"}),
                None,
                "tamper-open",
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE events SET event_hash=?2 WHERE engagement_id=?1",
                params![opened.engagement_id.to_string(), "f".repeat(64)],
            )
            .unwrap();
        let event: (String, String, String, String) = store
            .connection
            .query_row(
                "SELECT kind,payload_sha256,previous_hash,event_hash FROM events
                 WHERE engagement_id=?1",
                [opened.engagement_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_ne!(
            event.3,
            event_digest(&event.2, opened.engagement_id, &event.0, &event.1)
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identities_are_reserved_before_runtime_effects_and_results_are_durable() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open-key")
            .unwrap();
        let hired = store
            .hire(
                opened.engagement_id,
                &"b".repeat(64),
                "hire-key",
                |reservation| {
                    assert_eq!(
                        store_snapshot(&root, opened.engagement_id).specialist_run_id,
                        Some(reservation.specialist_run_id)
                    );
                    assert_eq!(reservation.hire_operation_id.get_version_num(), 7);
                    Ok(RuntimeOutcome::Accepted)
                },
            )
            .unwrap();
        let task = store
            .assign(
                opened.engagement_id,
                hired.specialist_run_id,
                &"c".repeat(64),
                None,
                "task-key",
                |reservation| {
                    assert_eq!(
                        store_snapshot(&root, opened.engagement_id).task_id,
                        Some(reservation.task_id)
                    );
                    (
                        RuntimeOutcome::Accepted,
                        Some(serde_json::json!({"summary":"clean","findings":[]})),
                    )
                },
            )
            .unwrap();
        assert_eq!(task.state, "result_ready");
        assert_eq!(
            store.collect(opened.engagement_id).unwrap()["summary"],
            "clean"
        );
        drop(store);
        let reopened = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        assert_eq!(
            reopened.snapshot(opened.engagement_id).unwrap().state,
            "result_ready"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn independent_engagements_may_publish_identical_checked_results() {
        let (mut store, root) = store();
        for number in 0..2 {
            let opened = store
                .open_engagement("workspace", &"a".repeat(64), &format!("open-{number}"))
                .unwrap();
            let hired = store
                .hire(
                    opened.engagement_id,
                    &"b".repeat(64),
                    &format!("hire-{number}"),
                    |_| Ok(RuntimeOutcome::Accepted),
                )
                .unwrap();
            store
                .assign(
                    opened.engagement_id,
                    hired.specialist_run_id,
                    &"c".repeat(64),
                    None,
                    &format!("task-{number}"),
                    |_| {
                        (
                            RuntimeOutcome::Accepted,
                            Some(serde_json::json!({"summary":"clean","findings":[]})),
                        )
                    },
                )
                .unwrap();
        }
        let artifacts: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(artifacts, 2);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v1_artifact_uniqueness_is_migrated_without_losing_rows() {
        fn table_shape(
            connection: &Connection,
            table: &str,
        ) -> Vec<(String, String, i64, Option<String>, i64)> {
            let mut statement = connection
                .prepare(
                    "SELECT name,type,\"notnull\",dflt_value,pk FROM pragma_table_info(?1) ORDER BY cid",
                )
                .unwrap();
            statement
                .query_map([table], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }

        let (store, root) = store();
        drop(store);
        let database = root.join("orchestration.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                r#"PRAGMA foreign_keys=OFF;
                 DROP TABLE delivery_receipts;
                 DROP TABLE artifacts;
                 CREATE TABLE artifacts(
                   artifact_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL UNIQUE,
                   result_sha256 TEXT NOT NULL UNIQUE,
                   canonical_json TEXT NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                 ) STRICT;
                 CREATE TABLE delivery_receipts(
                   sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                   task_id TEXT NOT NULL UNIQUE,
                   artifact_id TEXT NOT NULL,
                   delivered_at_ms INTEGER NOT NULL,
                   FOREIGN KEY(task_id) REFERENCES tasks(task_id),
                   FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                 ) STRICT;
                 INSERT INTO engagements(
                   engagement_id, workspace_id, controller_sha256, state,
                   specialist_run_id, task_id, result_sha256, result_json, revision
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000001', 'workspace',
                   'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                   'result_ready', '01900000-0000-7000-8000-000000000002',
                   '01900000-0000-7000-8000-000000000003',
                   'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                   '{"summary":"clean","findings":[]}', 4
                 );
                 INSERT INTO members(
                   specialist_run_id, engagement_id, hire_operation_id,
                   configuration_sha256, state
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000002',
                   '01900000-0000-7000-8000-000000000001',
                   '01900000-0000-7000-8000-000000000004',
                   'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                   'ready'
                 );
                 INSERT INTO tasks(
                   task_id, engagement_id, specialist_run_id, objective_sha256,
                   state, result_sha256
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000003',
                   '01900000-0000-7000-8000-000000000001',
                   '01900000-0000-7000-8000-000000000002',
                   'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                   'result_ready',
                   'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd'
                 );
                 INSERT INTO artifacts(
                   artifact_id, engagement_id, result_sha256, canonical_json
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000005',
                   '01900000-0000-7000-8000-000000000001',
                   'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                   '{"findings":[],"summary":"clean"}'
                 );
                 INSERT INTO delivery_receipts(
                   sequence, task_id, artifact_id, delivered_at_ms
                 ) VALUES(
                   7, '01900000-0000-7000-8000-000000000003',
                   '01900000-0000-7000-8000-000000000005', 123456
                 );
                 INSERT INTO engagements(
                   engagement_id, workspace_id, controller_sha256, state, revision
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000008', 'workspace',
                   'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                   'executing', 2
                 );
                 INSERT INTO members(
                   specialist_run_id, engagement_id, hire_operation_id,
                   configuration_sha256, state
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000009',
                   '01900000-0000-7000-8000-000000000008',
                   '01900000-0000-7000-8000-00000000000a',
                   'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                   'ready'
                 );
                 INSERT INTO tasks(
                   task_id, engagement_id, specialist_run_id, objective_sha256, state
                 ) VALUES(
                   '01900000-0000-7000-8000-00000000000b',
                   '01900000-0000-7000-8000-000000000008',
                   '01900000-0000-7000-8000-000000000009',
                   'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                   'accepted'
                 );
                 UPDATE metadata SET value='1' WHERE key='schema_version';"#,
            )
            .unwrap();
        drop(connection);
        let migrated = EngagementStore::open(&database).unwrap();
        let version: String = migrated
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "3");
        let artifact: (String, String, String, String) = migrated
            .connection
            .query_row(
                "SELECT artifact_id, engagement_id, result_sha256, canonical_json FROM artifacts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(artifact.0, "01900000-0000-7000-8000-000000000005");
        assert_eq!(artifact.1, "01900000-0000-7000-8000-000000000001");
        assert_eq!(artifact.2, "d".repeat(64));
        assert_eq!(artifact.3, r#"{"findings":[],"summary":"clean"}"#);
        let receipt: (i64, String, String, i64) = migrated
            .connection
            .query_row(
                "SELECT sequence, task_id, artifact_id, delivered_at_ms FROM delivery_receipts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(receipt.0, 7);
        assert_eq!(receipt.1, "01900000-0000-7000-8000-000000000003");
        assert_eq!(receipt.2, "01900000-0000-7000-8000-000000000005");
        assert_eq!(receipt.3, 123456);
        let migrated_result_state: String = migrated
            .connection
            .query_row(
                "SELECT state FROM tasks WHERE task_id='01900000-0000-7000-8000-000000000003'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated_result_state, "completed_not_delivered");
        let migrated_unknown: (String, String) = migrated
            .connection
            .query_row(
                "SELECT e.state,t.state FROM engagements e JOIN tasks t USING(engagement_id) WHERE e.engagement_id='01900000-0000-7000-8000-000000000008'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            migrated_unknown,
            (
                "interrupted_unknown".to_owned(),
                "interrupted_unknown".to_owned()
            )
        );
        migrated
            .connection
            .execute_batch(
                r#"INSERT INTO engagements(
                   engagement_id, workspace_id, controller_sha256, state, revision
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000006', 'workspace',
                   'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                   'open', 1
                 );
                 INSERT INTO artifacts(
                   artifact_id, engagement_id, result_sha256, canonical_json
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000007',
                   '01900000-0000-7000-8000-000000000006',
                   'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                   '{"findings":[],"summary":"clean"}'
                 );"#,
            )
            .unwrap();
        let violations: i64 = migrated
            .connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        let fresh = EngagementStore::open(&root.join("fresh.sqlite3")).unwrap();
        for table in [
            "engagements",
            "members",
            "tasks",
            "artifacts",
            "delivery_receipts",
            "operations",
            "events",
        ] {
            assert_eq!(
                table_shape(&migrated.connection, table),
                table_shape(&fresh.connection, table),
                "migrated {table} schema diverges from a fresh v3 store"
            );
        }
        drop(fresh);
        drop(migrated);
        let reopened = EngagementStore::open(&database).unwrap();
        let artifacts: i64 = reopened
            .connection
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(artifacts, 2);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_v1_migration_rolls_back_schema_and_rows() {
        let (store, root) = store();
        drop(store);
        let database = root.join("orchestration.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 DROP TABLE delivery_receipts;
                 DROP TABLE artifacts;
                 CREATE TABLE artifacts(
                   artifact_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL UNIQUE,
                   result_sha256 TEXT NOT NULL UNIQUE,
                   canonical_json TEXT NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                 ) STRICT;
                 CREATE TABLE delivery_receipts(
                   sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                   task_id TEXT NOT NULL UNIQUE,
                   artifact_id TEXT NOT NULL,
                   delivered_at_ms INTEGER NOT NULL,
                   FOREIGN KEY(task_id) REFERENCES tasks(task_id),
                   FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                 ) STRICT;
                 INSERT INTO artifacts(
                   artifact_id, engagement_id, result_sha256, canonical_json
                 ) VALUES(
                   '01900000-0000-7000-8000-000000000011',
                   '01900000-0000-7000-8000-000000000012',
                   'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
                   '{}'
                 );
                 UPDATE metadata SET value='1' WHERE key='schema_version';",
            )
            .unwrap();
        drop(connection);

        let error = match EngagementStore::open(&database) {
            Ok(_) => panic!("invalid v1 store unexpectedly migrated"),
            Err(error) => error,
        };
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert_eq!(
            error.details["reason"],
            "orchestration v1 migration violated a foreign key"
        );

        let connection = Connection::open(&database).unwrap();
        let version: String = connection
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "1");
        let artifacts: i64 = connection
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(artifacts, 1);
        let migrated_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name LIKE '%_v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated_tables, 0);
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_reviewer_flow_binds_the_specialist_and_checks_output() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open")
            .unwrap();
        let objective = "Review the current working tree for correctness.";
        let plan = reviewer_plan(objective);
        let hired = store
            .hire_reviewer(
                opened.engagement_id,
                &plan,
                "hire",
                |reservation, binding, plan| {
                    assert_eq!(
                        binding.aggregate_kind,
                        AggregateKind::ExternalSpecialistEngagement
                    );
                    assert_eq!(binding.aggregate_id, reservation.engagement_id);
                    assert_eq!(binding.operation_id, reservation.hire_operation_id);
                    assert_eq!(binding.member_kind, AggregateMemberKind::Specialist);
                    assert_eq!(
                        binding.role_reference.as_deref(),
                        Some(REVIEWER_ROLE_REFERENCE)
                    );
                    assert_eq!(
                        plan.agent_configuration.execution_lane,
                        ExecutionLane::SharedReadonly
                    );
                    RuntimeOutcome::Accepted
                },
            )
            .unwrap();
        let task = store
            .assign_review(
                opened.engagement_id,
                hired.specialist_run_id,
                objective,
                "task",
                |reservation, received| {
                    assert_eq!(reservation.specialist_run_id, hired.specialist_run_id);
                    assert_eq!(received, objective);
                    (
                        RuntimeOutcome::Accepted,
                        Some(serde_json::json!({"summary":"clean","findings":[]})),
                    )
                },
            )
            .unwrap();
        assert_eq!(task.state, "result_ready");
        assert_eq!(
            store.collect(opened.engagement_id).unwrap()["summary"],
            "clean"
        );

        let member_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM members", [], |row| row.get(0))
            .unwrap();
        let task_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .unwrap();
        let artifact_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .unwrap();
        let receipt_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM delivery_receipts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            (member_count, task_count, artifact_count, receipt_count),
            (1, 1, 1, 1)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v3_task_persists_the_complete_accepted_request() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open-v3")
            .unwrap();
        let plan = reviewer_plan("hire rationale only");
        let hired = store
            .hire_reviewer(opened.engagement_id, &plan, "hire-v3", |_, _, _| {
                RuntimeOutcome::Accepted
            })
            .unwrap();
        let task = SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "한글 brief\nsecond line".to_owned(),
            contexts: vec![TaskContext {
                id: "requirements".to_owned(),
                content: "exact accepted context".to_owned(),
                provenance: "approved spec".to_owned(),
            }],
            criteria: vec![TaskCriterion {
                id: "C-1".to_owned(),
                statement: "Role and task stay separate.".to_owned(),
                source_context_ids: vec!["requirements".to_owned()],
            }],
            expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
        };
        let accepted = serde_json::json!({
            "schema":"dolgorae-specialist-review-request/v3",
            "operation":"review_target",
            "deadline_seconds":60,
            "task":&task
        });
        let raw_report = serde_json::json!({
            "summary":"accepted",
            "findings":[
                {"severity":"P3","title":"later","description":"minor issue","path":null,"line_start":null,"line_end":null,"recommendation":"fix later","confidence":"low"},
                {"severity":"P1","title":"first","description":"important issue","path":"src/lib.rs","line_start":1,"line_end":1,"recommendation":"fix first","confidence":"high"}
            ],
            "criterion_assessments":[{
                "criterion_id":"C-1",
                "status":"met",
                "explanation":"The accepted task keeps the role separate.",
                "evidence":[{
                    "basis":"candidate",
                    "description":"The request is stored separately.",
                    "path":"src/engagement.rs",
                    "line_start":1,
                    "line_end":1,
                    "context_id":null
                }],
                "remaining_gap":null
            }],
            "evidence_limits":[],
            "overall_assessment":"requirements_met"
        });
        let expected_report =
            serde_json::to_value(validate_reviewer_output_v3(raw_report.clone(), &task).unwrap())
                .unwrap();
        let reserved = store
            .assign_task_v3(
                opened.engagement_id,
                hired.specialist_run_id,
                &accepted,
                &task,
                "task-v3",
                |_, _, prompt| {
                    assert!(prompt.contains("exact accepted context"));
                    assert!(prompt.contains("never candidate bytes"));
                    Ok((RuntimeOutcome::Accepted, Some(raw_report)))
                },
            )
            .unwrap();
        let stored: String = store
            .connection
            .query_row(
                "SELECT request_json FROM tasks WHERE task_id=?1",
                [reserved.task_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, canonical_string(&accepted).unwrap());
        assert!(stored.contains("한글 brief\\nsecond line"));
        assert!(stored.contains("exact accepted context"));

        let replayed = store
            .assign_task_v3(
                opened.engagement_id,
                hired.specialist_run_id,
                &accepted,
                &task,
                "task-v3",
                |_, _, _| panic!("an exact retry must not dispatch again"),
            )
            .unwrap();
        assert_eq!(replayed.task_id, reserved.task_id);
        let (created_at_ms, deadline_seconds): (Option<i64>, Option<i64>) = store
            .connection
            .query_row(
                "SELECT created_at_ms,deadline_seconds FROM tasks WHERE task_id=?1",
                [reserved.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(created_at_ms.is_some_and(|value| value > 0));
        assert_eq!(deadline_seconds, Some(60));
        store
            .connection
            .execute(
                "UPDATE tasks SET created_at_ms=0 WHERE task_id=?1",
                [reserved.task_id.to_string()],
            )
            .unwrap();
        assert_eq!(
            store
                .task_deadline_remaining(opened.engagement_id, reserved.task_id)
                .unwrap(),
            Some(Duration::ZERO)
        );

        let mut changed_task = task.clone();
        changed_task.contexts[0].content = "later mutable content".to_owned();
        let changed_accepted = serde_json::json!({
            "schema":"dolgorae-specialist-review-request/v3",
            "operation":"review_target",
            "task":&changed_task
        });
        assert_eq!(
            store
                .assign_task_v3(
                    opened.engagement_id,
                    hired.specialist_run_id,
                    &changed_accepted,
                    &changed_task,
                    "task-v3",
                    |_, _, _| panic!("a changed retry must not dispatch"),
                )
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        let collected = store.collect_review(opened.engagement_id).unwrap().output;
        assert_eq!(collected, expected_report);
        assert_eq!(collected["findings"][0]["severity"], "P1");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v3_task_rejects_invalid_output_before_artifact_commit() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open-invalid-v3")
            .unwrap();
        let plan = reviewer_plan("hire rationale only");
        let hired = store
            .hire_reviewer(opened.engagement_id, &plan, "hire-invalid-v3", |_, _, _| {
                RuntimeOutcome::Accepted
            })
            .unwrap();
        let task = SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "Assess completion.".to_owned(),
            contexts: vec![],
            criteria: vec![TaskCriterion {
                id: "C-1".to_owned(),
                statement: "The requirement is met.".to_owned(),
                source_context_ids: vec![],
            }],
            expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
        };
        let accepted = serde_json::json!({
            "schema":"dolgorae-specialist-review-request/v3",
            "operation":"review_target",
            "task":&task
        });
        let error = store
            .assign_task_v3(
                opened.engagement_id,
                hired.specialist_run_id,
                &accepted,
                &task,
                "invalid-task-v3",
                |_, _, _| {
                    Ok((
                        RuntimeOutcome::Accepted,
                        Some(serde_json::json!({"summary":"incomplete","findings":[]})),
                    ))
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "REVIEW_OUTPUT_INVALID");
        let artifact_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE engagement_id=?1",
                [opened.engagement_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(artifact_count, 0);
        let durable_error: Option<String> = store
            .connection
            .query_row(
                "SELECT safe_error_code FROM tasks WHERE engagement_id=?1",
                [opened.engagement_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(durable_error.as_deref(), Some("REVIEW_OUTPUT_INVALID"));
        drop(store);

        let mut reopened = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        let replay_error = reopened
            .assign_task_v3(
                opened.engagement_id,
                hired.specialist_run_id,
                &accepted,
                &task,
                "invalid-task-v3",
                |_, _, _| panic!("an invalid-output replay must not dispatch again"),
            )
            .unwrap_err();
        assert_eq!(replay_error, error);
        let artifact_count: i64 = reopened
            .connection
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE engagement_id=?1",
                [opened.engagement_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(artifact_count, 0);

        let callback_opened = reopened
            .open_engagement("workspace", &"d".repeat(64), "open-callback-invalid-v3")
            .unwrap();
        let callback_hired = reopened
            .hire_reviewer(
                callback_opened.engagement_id,
                &reviewer_plan("production-shaped invalid output"),
                "hire-callback-invalid-v3",
                |_, _, _| RuntimeOutcome::Accepted,
            )
            .unwrap();
        let malformed_turn = serde_json::json!({
            "status":"completed",
            "final_response":{"kind":"inline","text":"not json"}
        });
        let callback_error = reopened
            .assign_task_v3(
                callback_opened.engagement_id,
                callback_hired.specialist_run_id,
                &accepted,
                &task,
                "callback-invalid-task-v3",
                |_, _, _| {
                    crate::specialist::reviewer_turn_output_value(&malformed_turn)
                        .map(|output| (RuntimeOutcome::Accepted, Some(output)))
                },
            )
            .unwrap_err();
        assert_eq!(callback_error.code, "REVIEW_OUTPUT_INVALID");
        assert_eq!(
            callback_error.details,
            serde_json::json!({
                "reason":"Reviewer output is not JSON",
                "required_action":"none"
            })
        );
        let callback_row: (String, Option<String>, Option<String>, Option<String>) = reopened
            .connection
            .query_row(
                "SELECT state,artifact_id,result_sha256,safe_error_code FROM tasks \
                 WHERE engagement_id=?1",
                [callback_opened.engagement_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            callback_row,
            (
                "failed".to_owned(),
                None,
                None,
                Some("REVIEW_OUTPUT_INVALID".to_owned())
            )
        );
        drop(reopened);

        let mut replayed = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        let callback_replay_error = replayed
            .assign_task_v3(
                callback_opened.engagement_id,
                callback_hired.specialist_run_id,
                &accepted,
                &task,
                "callback-invalid-task-v3",
                |_, _, _| panic!("a durable callback error must not dispatch again"),
            )
            .unwrap_err();
        assert_eq!(callback_replay_error, callback_error);
        drop(replayed);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_reviewer_output_is_failed_before_collection() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open")
            .unwrap();
        let plan = reviewer_plan("Review safely.");
        let hired = store
            .hire_reviewer(opened.engagement_id, &plan, "hire", |_, _, _| {
                RuntimeOutcome::Accepted
            })
            .unwrap();
        let task = store
            .assign_review(
                opened.engagement_id,
                hired.specialist_run_id,
                "Review safely.",
                "task",
                |_, _| {
                    (
                        RuntimeOutcome::Accepted,
                        Some(serde_json::json!({"summary":"missing findings"})),
                    )
                },
            )
            .unwrap();
        assert_eq!(task.state, "failed");
        assert_eq!(
            store.collect(opened.engagement_id).unwrap_err().code,
            "STATE_CONFLICT"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn store_snapshot(root: &Path, id: Uuid) -> EngagementSnapshot {
        EngagementStore::open(&root.join("orchestration.sqlite3"))
            .unwrap()
            .snapshot(id)
            .unwrap()
    }

    #[test]
    fn exact_replay_preserves_identity_and_drift_conflicts() {
        let (mut store, root) = store();
        let first = store
            .open_engagement("workspace", &"a".repeat(64), "same")
            .unwrap();
        let replay = store
            .open_engagement("workspace", &"a".repeat(64), "same")
            .unwrap();
        assert_eq!(first, replay);
        assert_eq!(
            store
                .open_engagement("changed", &"a".repeat(64), "same")
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_task_is_terminally_quarantined_and_never_collects() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open")
            .unwrap();
        let hired = store
            .hire(opened.engagement_id, &"b".repeat(64), "hire", |_| {
                Ok(RuntimeOutcome::Accepted)
            })
            .unwrap();
        let task = store
            .assign(
                opened.engagement_id,
                hired.specialist_run_id,
                &"c".repeat(64),
                None,
                "task",
                |_| (RuntimeOutcome::Unknown, None),
            )
            .unwrap();
        assert_eq!(task.state, "interrupted_unknown");
        assert_eq!(
            store.collect(opened.engagement_id).unwrap_err().code,
            "STATE_CONFLICT"
        );
        assert_eq!(
            store
                .cancel(opened.engagement_id, "cancel")
                .unwrap_err()
                .code,
            "STATE_CONFLICT"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_hire_is_observable_as_terminal_quarantine() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open")
            .unwrap();
        let quarantined = store
            .hire(opened.engagement_id, &"b".repeat(64), "hire", |_| {
                Ok(RuntimeOutcome::Unknown)
            })
            .unwrap();
        assert_eq!(quarantined.state, "recovery_required");
        assert_eq!(
            store
                .await_terminal(opened.engagement_id, Duration::from_secs(1))
                .unwrap()
                .state,
            quarantined.state
        );
        assert_eq!(
            store
                .cancel(opened.engagement_id, "cancel")
                .unwrap_err()
                .code,
            "STATE_CONFLICT"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn release_and_close_require_terminal_one_shot_result() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open")
            .unwrap();
        let hired = store
            .hire(opened.engagement_id, &"b".repeat(64), "hire", |_| {
                Ok(RuntimeOutcome::Accepted)
            })
            .unwrap();
        store
            .assign(
                opened.engagement_id,
                hired.specialist_run_id,
                &"c".repeat(64),
                None,
                "task",
                |_| {
                    (
                        RuntimeOutcome::Accepted,
                        Some(serde_json::json!({"ok":true})),
                    )
                },
            )
            .unwrap();
        let released = store.release(opened.engagement_id, "release").unwrap();
        assert_eq!(
            store.release(opened.engagement_id, "release").unwrap(),
            released
        );
        let closed = store.close(opened.engagement_id, "close").unwrap();
        assert_eq!(store.close(opened.engagement_id, "close").unwrap(), closed);
        assert_eq!(
            store.snapshot(opened.engagement_id).unwrap().state,
            "closed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_commit_faults_are_atomic_and_exact_replay_recovers_after_commit() {
        for barrier in [
            EngagementBarrier::BeforeOpenCommit,
            EngagementBarrier::AfterOpenCommit,
        ] {
            let root = std::env::temp_dir()
                .join(format!("dolgorae-engagement-open-fault-{}", Uuid::now_v7()));
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .open_engagement("workspace", &"a".repeat(64), "open")
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            let result = recovered
                .open_engagement("workspace", &"a".repeat(64), "open")
                .unwrap();
            assert_eq!(result.state, "open");
            assert_eq!(
                recovered
                    .open_engagement("workspace", &"a".repeat(64), "open")
                    .unwrap(),
                result
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn hire_commit_faults_never_duplicate_or_replay_uncertain_publication() {
        for barrier in [
            EngagementBarrier::BeforeHireReservationCommit,
            EngagementBarrier::AfterHireReservationCommit,
            EngagementBarrier::BeforeHireOutcomeCommit,
            EngagementBarrier::AfterHireOutcomeCommit,
        ] {
            let (mut initial, root) = store();
            let opened = initial
                .open_engagement("workspace", &"a".repeat(64), "open")
                .unwrap();
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            let published = std::sync::atomic::AtomicUsize::new(0);
            let mut reserved_run_id = None;
            assert_eq!(
                faulted
                    .hire(
                        opened.engagement_id,
                        &"b".repeat(64),
                        "hire",
                        |reservation| {
                            reserved_run_id = Some(reservation.specialist_run_id);
                            published.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            Ok(RuntimeOutcome::Accepted)
                        }
                    )
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            let calls_before_recovery = published.load(std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                reserved_run_id.is_some(),
                matches!(
                    barrier,
                    EngagementBarrier::BeforeHireOutcomeCommit
                        | EngagementBarrier::AfterHireOutcomeCommit
                ),
                "the coordinator can retain the reserved Run exactly when publication ran"
            );
            drop(faulted);
            let mut recovered = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            let result = recovered
                .hire(opened.engagement_id, &"b".repeat(64), "hire", |_| {
                    published.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(RuntimeOutcome::Accepted)
                })
                .unwrap();
            assert!(matches!(
                result.state.as_str(),
                "ready" | "recovery_required"
            ));
            assert_eq!(
                published.load(std::sync::atomic::Ordering::SeqCst),
                if barrier == EngagementBarrier::BeforeHireReservationCommit {
                    1
                } else {
                    calls_before_recovery
                }
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn rejected_hire_is_terminal_and_can_be_released_and_closed() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open")
            .unwrap();
        let rejected = store
            .hire(opened.engagement_id, &"b".repeat(64), "hire", |_| {
                Ok(RuntimeOutcome::Rejected)
            })
            .unwrap();
        assert_eq!(rejected.state, "failed");
        assert_eq!(
            store
                .release(opened.engagement_id, "release")
                .unwrap()
                .state,
            "released"
        );
        assert_eq!(
            store.close(opened.engagement_id, "close").unwrap().state,
            "closed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_commit_faults_preserve_results_or_quarantine_unknown_work() {
        for barrier in [
            EngagementBarrier::BeforeTaskReservationCommit,
            EngagementBarrier::AfterTaskReservationCommit,
            EngagementBarrier::BeforeTaskOutcomeCommit,
            EngagementBarrier::AfterTaskOutcomeCommit,
        ] {
            let (mut initial, root) = store();
            let opened = initial
                .open_engagement("workspace", &"a".repeat(64), "open")
                .unwrap();
            let hired = initial
                .hire(opened.engagement_id, &"b".repeat(64), "hire", |_| {
                    Ok(RuntimeOutcome::Accepted)
                })
                .unwrap();
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            let executed = std::sync::atomic::AtomicUsize::new(0);
            assert_eq!(
                faulted
                    .assign(
                        opened.engagement_id,
                        hired.specialist_run_id,
                        &"c".repeat(64),
                        None,
                        "task",
                        |_| {
                            executed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            (
                                RuntimeOutcome::Accepted,
                                Some(serde_json::json!({"ok":true})),
                            )
                        },
                    )
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            let calls_before_recovery = executed.load(std::sync::atomic::Ordering::SeqCst);
            drop(faulted);
            let mut recovered = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            let result = recovered
                .assign(
                    opened.engagement_id,
                    hired.specialist_run_id,
                    &"c".repeat(64),
                    None,
                    "task",
                    |_| {
                        executed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        (
                            RuntimeOutcome::Accepted,
                            Some(serde_json::json!({"ok":true})),
                        )
                    },
                )
                .unwrap();
            assert!(matches!(
                result.state.as_str(),
                "result_ready" | "interrupted_unknown"
            ));
            assert_eq!(
                executed.load(std::sync::atomic::Ordering::SeqCst),
                if barrier == EngagementBarrier::BeforeTaskReservationCommit {
                    1
                } else {
                    calls_before_recovery
                }
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn lifecycle_commit_faults_are_atomic_and_replayable() {
        for barrier in [
            EngagementBarrier::BeforeLifecycleCommit,
            EngagementBarrier::AfterLifecycleCommit,
        ] {
            let (mut initial, root) = store();
            let opened = initial
                .open_engagement("workspace", &"a".repeat(64), "open")
                .unwrap();
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .cancel(opened.engagement_id, "cancel")
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            let cancelled = recovered.cancel(opened.engagement_id, "cancel").unwrap();
            assert_eq!(cancelled.state, "cancelled");
            assert_eq!(
                recovered.cancel(opened.engagement_id, "cancel").unwrap(),
                cancelled
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn delivery_commit_faults_preserve_one_immutable_artifact_and_receipt() {
        for barrier in [
            EngagementBarrier::BeforeDeliveryCommit,
            EngagementBarrier::AfterDeliveryCommit,
        ] {
            let (mut initial, root) = store();
            let opened = initial
                .open_engagement("workspace", &"a".repeat(64), "open")
                .unwrap();
            let hired = initial
                .hire(opened.engagement_id, &"b".repeat(64), "hire", |_| {
                    Ok(RuntimeOutcome::Accepted)
                })
                .unwrap();
            initial
                .assign(
                    opened.engagement_id,
                    hired.specialist_run_id,
                    &"c".repeat(64),
                    None,
                    "task",
                    |_| {
                        (
                            RuntimeOutcome::Accepted,
                            Some(serde_json::json!({"summary":"clean","findings":[]})),
                        )
                    },
                )
                .unwrap();
            drop(initial);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted.collect(opened.engagement_id).unwrap_err().code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut recovered = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            assert_eq!(
                recovered.collect(opened.engagement_id).unwrap()["summary"],
                "clean"
            );
            assert_eq!(
                recovered.collect(opened.engagement_id).unwrap()["summary"],
                "clean"
            );
            let counts: (i64, i64) = recovered
                .connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM artifacts),(SELECT COUNT(*) FROM delivery_receipts)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(counts, (1, 1));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn await_timeout_and_cancel_have_bounded_terminal_semantics() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open")
            .unwrap();
        let error = store
            .await_terminal(opened.engagement_id, Duration::from_millis(1))
            .unwrap_err();
        assert_eq!(error.code, "ENGAGEMENT_TIMEOUT");
        assert!(!error.retryable);
        assert_eq!(
            error.details,
            serde_json::json!({"required_action":"cancel_or_await_again"})
        );
        let cancelled = store.cancel(opened.engagement_id, "cancel").unwrap();
        assert_eq!(cancelled.state, "cancelled");
        assert_eq!(
            store
                .await_terminal(opened.engagement_id, Duration::from_secs(1))
                .unwrap(),
            cancelled
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sqlite_durability_pragmas_and_event_chain_are_enforced() {
        let (mut store, root) = store();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(root.join("orchestration.sqlite3"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open")
            .unwrap();
        store.cancel(opened.engagement_id, "cancel").unwrap();
        let journal: String = store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = store
            .connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = store
            .connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(synchronous, 2);

        let mut statement = store
            .connection
            .prepare(
                "SELECT engagement_id,kind,payload_sha256,previous_hash,event_hash FROM events ORDER BY sequence",
            )
            .unwrap();
        let events = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut previous = "0".repeat(64);
        for (engagement_id, kind, payload_sha256, recorded_previous, event_hash) in events {
            assert_eq!(recorded_previous, previous);
            previous = sha256_hex(
                format!("{recorded_previous}\0{engagement_id}\0{kind}\0{payload_sha256}")
                    .as_bytes(),
            );
            assert_eq!(event_hash, previous);
        }
        drop(statement);
        std::fs::remove_dir_all(root).unwrap();
    }
}
