//! Durable one-shot External Specialist Engagement authority.

use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::run::{AggregateBinding, agent_configuration_digest};
use crate::specialist::{ReviewerRuntimePlan, validate_reviewer_output};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;

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
}

pub struct EngagementStore {
    connection: Connection,
    faults: Arc<dyn EngagementFaultInjector>,
}

impl EngagementStore {
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
        }
        let connection = Connection::open(path).map_err(internal)?;
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
                   revision INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS operations(
                   operation TEXT NOT NULL,
                   idempotency_key TEXT NOT NULL,
                   request_sha256 TEXT NOT NULL,
                   response_json TEXT NOT NULL,
                   PRIMARY KEY(operation,idempotency_key)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS members(
                   specialist_run_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL UNIQUE,
                   hire_operation_id TEXT NOT NULL UNIQUE,
                   configuration_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS tasks(
                   task_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL UNIQUE,
                   specialist_run_id TEXT NOT NULL,
                   objective_sha256 TEXT NOT NULL,
                   state TEXT NOT NULL,
                   result_sha256 TEXT,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id),
                   FOREIGN KEY(specialist_run_id) REFERENCES members(specialist_run_id)
                 ) STRICT;
                 CREATE TABLE IF NOT EXISTS artifacts(
                   artifact_id TEXT PRIMARY KEY,
                   engagement_id TEXT NOT NULL UNIQUE,
                   result_sha256 TEXT NOT NULL UNIQUE,
                   canonical_json TEXT NOT NULL,
                   FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
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
        if observed != SCHEMA_VERSION.to_string() {
            return Err(MachineError::new(
                "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                "orchestration schema is not supported",
                false,
                serde_json::json!({"observed":observed}),
            ));
        }
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
                "INSERT INTO engagements VALUES(?1,?2,?3,'open',NULL,NULL,NULL,NULL,1)",
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
                    &replay,
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
            require_state(&transaction, engagement_id, &["open"])?;
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
                    "INSERT INTO members VALUES(?1,?2,?3,?4,'provisioning')",
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
            RuntimeOutcome::Rejected => "recovery_required",
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
            &final_result,
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
        checked(objective, 65_536, "objective")?;
        let objective_sha256 = sha256_hex(objective.as_bytes());
        self.assign(
            engagement_id,
            specialist_run_id,
            &objective_sha256,
            idempotency_key,
            |reservation| {
                let (outcome, value) = execute(reservation, objective);
                if outcome == RuntimeOutcome::Accepted {
                    match value.and_then(|value| {
                        validate_reviewer_output(value)
                            .ok()
                            .and_then(|output| serde_json::to_value(output).ok())
                    }) {
                        Some(value) => (RuntimeOutcome::Accepted, Some(value)),
                        None => (RuntimeOutcome::Rejected, None),
                    }
                } else {
                    (outcome, None)
                }
            },
        )
    }

    fn assign<F>(
        &mut self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        objective_sha256: &str,
        idempotency_key: &str,
        execute: F,
    ) -> Result<TaskReservation, MachineError>
    where
        F: FnOnce(&TaskReservation) -> (RuntimeOutcome, Option<Value>),
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
                    &replay,
                )?;
            }
            return Ok(replay);
        }
        let task_id = Uuid::now_v7();
        let reserved = TaskReservation {
            engagement_id,
            task_id,
            specialist_run_id,
            state: "accepted".to_owned(),
        };
        {
            let faults = Arc::clone(&self.faults);
            let transaction = self.transaction()?;
            require_state(&transaction, engagement_id, &["ready"])?;
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
                    "INSERT INTO tasks VALUES(?1,?2,?3,?4,'accepted',NULL)",
                    params![
                        task_id.to_string(),
                        engagement_id.to_string(),
                        specialist_run_id.to_string(),
                        objective_sha256
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
        let (outcome, result) = execute(&reserved);
        match outcome {
            RuntimeOutcome::Accepted => {
                let result = result.ok_or_else(|| conflict("accepted task has no result"))?;
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
                        "INSERT INTO artifacts VALUES(?1,?2,?3,?4)",
                        params![
                            artifact_id.to_string(),
                            engagement_id.to_string(),
                            result_sha256,
                            canonical_json
                        ],
                    )
                    .map_err(internal)?;
                transaction
                    .execute(
                        "UPDATE tasks SET state='result_ready',result_sha256=?2 WHERE engagement_id=?1",
                        params![engagement_id.to_string(), result_sha256],
                    )
                    .map_err(internal)?;
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
                    ..reserved
                };
                self.finish_operation(
                    engagement_id,
                    "failed",
                    "task_rejected",
                    "assign",
                    idempotency_key,
                    &final_result,
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
                    &final_result,
                )?;
                Ok(final_result)
            }
        }
    }

    pub fn collect(&mut self, engagement_id: Uuid) -> Result<Value, MachineError> {
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
        serde_json::from_str(&result).map_err(internal)
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
        self.connection
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
        require_state(&transaction, engagement_id, allowed)?;
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
        response: &T,
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
                transaction
                    .execute(
                        "UPDATE tasks SET state=?2 WHERE engagement_id=?1",
                        params![engagement_id.to_string(), state],
                    )
                    .map_err(internal)?;
            }
            _ => {}
        }
        append_event(&transaction, engagement_id, kind, state)?;
        update_response(&transaction, operation, idempotency_key, response)?;
        commit_with(&faults, transaction, before, after)
    }
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
            "SELECT request_sha256,response_json FROM operations WHERE operation=?1 AND idempotency_key=?2",
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

fn record<T: Serialize>(
    transaction: &Transaction<'_>,
    operation: &str,
    key: &str,
    request: &str,
    response: &T,
) -> Result<(), MachineError> {
    transaction
        .execute(
            "INSERT INTO operations VALUES(?1,?2,?3,?4)",
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
            "UPDATE operations SET response_json=?3 WHERE operation=?1 AND idempotency_key=?2",
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
    let event_hash =
        sha256_hex(format!("{previous}\0{}\0{kind}\0{payload_sha256}", engagement_id).as_bytes());
    let created_at_ms = unix_time_ms()?;
    transaction.execute(
        "INSERT INTO events(engagement_id,kind,payload_sha256,previous_hash,event_hash,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6)",
        params![engagement_id.to_string(), kind, payload_sha256, previous, event_hash, created_at_ms],
    ).map_err(internal)?;
    Ok(())
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

fn require_state(
    transaction: &Transaction<'_>,
    id: Uuid,
    allowed: &[&str],
) -> Result<(), MachineError> {
    require_current(transaction, id, allowed)
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
        .map_err(internal)
}

fn digest_value(value: &Value) -> Result<String, MachineError> {
    Ok(sha256_hex(&canonical_json(value)?))
}

fn canonical_json(value: &Value) -> Result<Vec<u8>, MachineError> {
    let text = serde_json::to_string(value).map_err(internal)?;
    canonicalize(&parse(&text).map_err(internal)?).map_err(internal)
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
    use crate::domain::{AggregateKind, ExecutionLane};
    use crate::run::{AggregateMemberKind, ExecutableIdentity, ProfileSnapshot};
    use crate::specialist::{REVIEWER_ROLE_REFERENCE, ReviewerRuntimeRequest};
    use crate::workspace::LosslessPath;
    use std::collections::BTreeMap;

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

    fn reviewer_plan(objective: &str) -> ReviewerRuntimePlan {
        let mut profile = ProfileSnapshot {
            schema_version: 1,
            profile_name: "reviewer".to_owned(),
            canonical_codex_home: "/tmp/codex-home".to_owned(),
            normalized_argv: vec!["/usr/bin/codex".to_owned()],
            launch_cwd_policy: "application_support_profile_root".to_owned(),
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
            codex_version: "0.149.0".to_owned(),
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
                objective: objective.to_owned(),
                required_capabilities: vec!["thread_read".to_owned()],
            },
        )
        .unwrap()
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
            assert_eq!(
                faulted
                    .hire(opened.engagement_id, &"b".repeat(64), "hire", |_| {
                        published.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(RuntimeOutcome::Accepted)
                    })
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            let calls_before_recovery = published.load(std::sync::atomic::Ordering::SeqCst);
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
        assert_eq!(
            store
                .await_terminal(opened.engagement_id, Duration::from_millis(1))
                .unwrap_err()
                .code,
            "ENGAGEMENT_TIMEOUT"
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
