//! Durable transport-independent Orchestrated Session authority.

use crate::domain::{ControllerIdentity, ControllerKind};
use crate::engagement::EngagementStore;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::{MachineError, new_uuid_v7};
use crate::run::{
    AggregateBinding, AggregateMemberKind, ControllerBinding, controller_capability_digest,
};
use crate::specialist_policy::{
    InstalledSpecialistPolicy, InstalledSpecialistRole, validate_installed_policy,
};
use crate::workspace::{
    SystemWorkspacePlatform, atomic_create, create_directory, open_relative_nofollow,
    sync_directory, verify_secure_directory,
};
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OrchestrationBarrier {
    BeforeSessionCommit,
    AfterSessionCommit,
    BeforePrimaryIntent,
    AfterPrimaryIntent,
    BeforePrimaryPublication,
    AfterPrimaryPublication,
    BeforeSpawnReservation,
    AfterSpawnReservation,
    BeforeWorkerPublication,
    AfterWorkerPublication,
    BeforeThreadCreation,
    AfterThreadCreation,
    BeforeTaskDispatch,
    AfterTaskDispatch,
    BeforeResultAppend,
    AfterResultAppend,
    BeforeDeliveryReceipt,
    AfterDeliveryReceipt,
}

pub trait OrchestrationFaultInjector: Send + Sync {
    fn check(&self, barrier: OrchestrationBarrier) -> Result<(), MachineError>;
}

#[derive(Default)]
pub struct NoOrchestrationFaults;

impl OrchestrationFaultInjector for NoOrchestrationFaults {
    fn check(&self, _barrier: OrchestrationBarrier) -> Result<(), MachineError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestratedSessionSnapshot {
    pub session_id: Uuid,
    pub bootstrap_operation_id: Uuid,
    pub root_run_id: Uuid,
    pub workspace_id: String,
    pub status: String,
    pub composition_state: String,
    pub approval_policy: String,
    pub specialist_policy: InstalledSpecialistPolicy,
    pub specialist_policy_sha256: String,
    pub revision: u64,
    pub members: Vec<BrokeredMemberSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokeredMemberSnapshot {
    pub run_id: Uuid,
    pub parent_run_id: Uuid,
    pub role_ref: String,
    pub role_snapshot: InstalledSpecialistRole,
    pub role_snapshot_sha256: String,
    pub agent_configuration: crate::run::AgentConfigurationSnapshot,
    pub agent_configuration_sha256: String,
    pub controller_binding: ControllerBinding,
    pub spawn_operation_id: Uuid,
    pub spawned_by_turn_id: Option<String>,
    pub membership_state: String,
    pub actor_residency: String,
    pub activation_policy: String,
    pub requested_access: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecialistOperationSnapshot {
    pub operation_id: Uuid,
    pub role_ref: String,
    pub state: String,
    pub specialist_run_id: Option<Uuid>,
    pub approval_request_id: Option<Uuid>,
    pub reused: bool,
    pub safe_error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecialistTaskSnapshot {
    pub task_id: Uuid,
    pub specialist_run_id: Uuid,
    pub state: String,
    pub target_turn_id: Option<String>,
    pub result: Option<Value>,
    pub result_artifact_ref: Option<Uuid>,
    pub result_sha256: Option<String>,
    pub safe_error_code: Option<String>,
    pub delivery_sequence: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimaryCallContext {
    pub session_id: Uuid,
    pub source_run_id: Uuid,
    pub source_turn_id: String,
    pub source_tool_call_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSpecialist {
    pub role_ref: String,
    pub objective: String,
    pub expected_output: Vec<String>,
    pub requested_access: String,
    pub deadline_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignSpecialistTask {
    pub specialist_run_id: Uuid,
    pub objective: String,
    pub context_refs: Vec<Uuid>,
    pub expected_output: Vec<String>,
    pub requested_access: String,
    pub deadline_seconds: u64,
}

pub struct BrokerCredential {
    capability: Zeroizing<[u8; 32]>,
    pub binding: ControllerBinding,
    path: PathBuf,
}

impl BrokerCredential {
    fn create(root: &Path, uid: u32, run_id: Uuid) -> Result<Self, MachineError> {
        ensure_credential_root(root, uid)?;
        let mut capability = Zeroizing::new([0_u8; 32]);
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut *capability))
            .map_err(internal)?;
        let binding = ControllerBinding {
            identity: ControllerIdentity {
                controller_id: new_uuid_v7(),
                kind: ControllerKind::Automation,
                instance_id: format!("orchestration-broker-{}", new_uuid_v7()),
                subject_id: None,
                generation: 1,
            },
            capability_sha256: controller_capability_digest(&capability),
        };
        let credential = Self {
            capability,
            binding,
            path: root.join(format!("{run_id}.json")),
        };
        let encoded = Zeroizing::new(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*credential.capability),
        );
        let wire = BrokerCredentialWireRef {
            schema_version: 1,
            run_id,
            binding: &credential.binding,
            capability: &encoded,
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&wire).map_err(internal)?);
        atomic_create(&SystemWorkspacePlatform, &credential.path, &bytes, 0o600)
            .map_err(internal)?;
        Ok(credential)
    }

    fn load(root: &Path, uid: u32, run_id: Uuid) -> Result<Self, MachineError> {
        verify_secure_directory(root, uid)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)
            .map_err(internal)?;
        let filename = format!("{run_id}.json");
        let file =
            open_relative_nofollow(&directory, OsStr::new(&filename), false).map_err(internal)?;
        let metadata = file.metadata().map_err(internal)?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() > 4_096
        {
            return Err(integrity("broker credential carrier is unsafe"));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(4_097).read_to_end(&mut bytes).map_err(internal)?;
        if bytes.len() > 4_096 {
            return Err(integrity("broker credential carrier exceeds its bound"));
        }
        let wire: BrokerCredentialWire = serde_json::from_slice(&bytes)
            .map_err(|_| integrity("broker credential carrier is invalid"))?;
        if wire.schema_version != 1
            || wire.run_id != run_id
            || wire.binding.identity.kind != ControllerKind::Automation
            || wire.binding.identity.generation != 1
        {
            return Err(integrity("broker credential carrier identity is invalid"));
        }
        let mut capability = Zeroizing::new([0_u8; 32]);
        let count = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode_slice(&wire.capability, &mut *capability)
            .map_err(|_| integrity("broker credential capability is invalid"))?;
        if count != 32
            || base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*capability)
                != wire.capability
            || controller_capability_digest(&capability) != wire.binding.capability_sha256
        {
            return Err(integrity("broker credential capability binding is invalid"));
        }
        Ok(Self {
            capability,
            binding: wire.binding,
            path: root.join(filename),
        })
    }

    fn remove(self) -> Result<(), MachineError> {
        fs::remove_file(&self.path).map_err(internal)?;
        sync_directory(
            self.path
                .parent()
                .ok_or_else(|| integrity("broker credential carrier has no parent"))?,
        )
        .map_err(internal)
    }

    #[must_use]
    pub fn capability(&self) -> &[u8; 32] {
        &self.capability
    }
}

#[derive(Serialize)]
struct BrokerCredentialWireRef<'a> {
    schema_version: u32,
    run_id: Uuid,
    binding: &'a ControllerBinding,
    capability: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerCredentialWire {
    schema_version: u32,
    run_id: Uuid,
    binding: ControllerBinding,
    capability: String,
}

fn ensure_credential_root(root: &Path, uid: u32) -> Result<(), MachineError> {
    let parent = root
        .parent()
        .ok_or_else(|| integrity("broker credential root has no parent"))?;
    verify_secure_directory(parent, uid)?;
    if !root.exists() {
        create_directory(root, 0o700).map_err(internal)?;
    }
    verify_secure_directory(root, uid)
}

impl Drop for BrokerCredential {
    fn drop(&mut self) {
        self.capability.zeroize();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterFailure {
    Rejected(String),
    Unknown,
}

pub trait OrchestrationAdapter {
    /// Idempotently creates the typed Primary `user_input` interaction keyed by
    /// `approval_request_id`. An unknown response must be safe to retry with
    /// the same identities and request.
    fn publish_approval_request(
        &mut self,
        session_id: Uuid,
        operation_id: Uuid,
        approval_request_id: Uuid,
        request: &RequestSpecialist,
    ) -> Result<(), AdapterFailure>;
    fn publish_specialist(
        &mut self,
        plan: &BrokeredRunPlan,
        credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure>;
    fn create_thread(
        &mut self,
        plan: &BrokeredRunPlan,
        credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure>;
    fn dispatch_task(
        &mut self,
        member: &BrokeredMemberSnapshot,
        request: &AssignSpecialistTask,
        credential: &BrokerCredential,
    ) -> Result<CompletedTask, AdapterFailure>;
    fn cancel_task(
        &mut self,
        task: &SpecialistTaskSnapshot,
        credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure>;
    fn release_specialist(
        &mut self,
        member: &BrokeredMemberSnapshot,
        credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure>;
    fn release_writer(&mut self, run_id: Uuid) -> Result<(), AdapterFailure>;
    fn verify_writer_none(&mut self) -> Result<(), AdapterFailure>;
    fn acquire_writer(&mut self, run_id: Uuid) -> Result<(), AdapterFailure>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrokeredRunPlan {
    pub session_id: Uuid,
    pub run_id: Uuid,
    pub aggregate_binding: AggregateBinding,
    pub role: InstalledSpecialistRole,
    pub requested_access: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletedTask {
    pub turn_id: String,
    pub result: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum TargetSelector {
    Run { run_id: Uuid },
    Role { role_ref: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ToolRequest {
    RequestSpecialist {
        role_ref: String,
        objective: String,
        expected_output: Vec<String>,
        requested_access: String,
        deadline_seconds: u64,
    },
    AwaitSpecialistOperations {
        operation_ids: Vec<Uuid>,
        return_when: String,
        transport_wait_seconds: u64,
    },
    ListSpecialists,
    AssignSpecialistTask {
        target: TargetSelector,
        objective: String,
        context_refs: Vec<Uuid>,
        expected_output: Vec<String>,
        execution_intent: String,
        blocking: bool,
        deadline_seconds: u64,
    },
    AwaitSpecialistTasks {
        task_ids: Vec<Uuid>,
        return_when: String,
        transport_wait_seconds: u64,
    },
    CollectSpecialistResults {
        limit: usize,
    },
    CancelSpecialistTask {
        task_id: Uuid,
        reason: String,
    },
    ReleaseSpecialist {
        run_id: Uuid,
        reason: String,
    },
}

/// Private run-bound tool dispatcher. The trusted transport supplies the
/// source identities and idempotency key in `PrimaryCallContext`; the model
/// payload contains none of those authorities.
pub struct PrimaryOrchestrationService<'a, A> {
    pub store: &'a mut OrchestrationStore,
    pub adapter: &'a mut A,
}

impl<A: OrchestrationAdapter> PrimaryOrchestrationService<'_, A> {
    pub fn dispatch(
        &mut self,
        context: &PrimaryCallContext,
        payload: &Value,
    ) -> Result<Value, MachineError> {
        validate_context(context)?;
        self.store.authorize_primary(context)?;
        let request_text = serde_json::to_string(payload).map_err(internal)?;
        let canonical = canonicalize(&parse(&request_text).map_err(internal)?).map_err(internal)?;
        let request_sha256 = sha256_hex(&canonical);
        if let Some(replay) = self.store.tool_result(context, &request_sha256)? {
            return Ok(replay);
        }
        validate_tool_request_shape(payload)?;
        let request: ToolRequest = serde_json::from_slice(&canonical).map_err(|_| {
            MachineError::invalid_argument(
                "tool_payload",
                "payload does not match the private orchestration tool contract",
            )
        })?;
        let response = self.execute(context, request)?;
        self.store
            .record_tool_result(context, &request_sha256, &response)?;
        Ok(response)
    }

    fn execute(
        &mut self,
        context: &PrimaryCallContext,
        request: ToolRequest,
    ) -> Result<Value, MachineError> {
        match request {
            ToolRequest::RequestSpecialist {
                role_ref,
                objective,
                expected_output,
                requested_access,
                deadline_seconds,
            } => {
                let operation = self.store.request_specialist(
                    context,
                    &RequestSpecialist {
                        role_ref,
                        objective,
                        expected_output,
                        requested_access,
                        deadline_seconds,
                    },
                    self.adapter,
                )?;
                Ok(serde_json::json!({
                    "operation":"request_specialist_result",
                    "specialist_operation":operation,
                }))
            }
            ToolRequest::AwaitSpecialistOperations {
                operation_ids,
                return_when,
                transport_wait_seconds,
            } => {
                validate_wait(&return_when, transport_wait_seconds)?;
                let operations = self
                    .store
                    .specialist_operations(context.session_id, &operation_ids)?;
                let pending = operations
                    .iter()
                    .filter(|operation| {
                        matches!(
                            operation.state.as_str(),
                            "awaiting_approval" | "requested" | "provisioning"
                        )
                    })
                    .map(|operation| operation.operation_id)
                    .collect::<Vec<_>>();
                Ok(serde_json::json!({
                    "operation":"await_specialist_operations_result",
                    "operations":operations,
                    "pending":pending,
                }))
            }
            ToolRequest::ListSpecialists => {
                let specialists = self
                    .store
                    .members(context.session_id)?
                    .into_iter()
                    .map(|member| {
                        serde_json::json!({
                            "run_id":member.run_id,
                            "role_ref":member.role_ref,
                            "membership_state":member.membership_state,
                            "actor_residency":member.actor_residency,
                            "task_state":self.store.member_task_state(member.run_id).unwrap_or_else(|_| "unavailable".to_owned()),
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(serde_json::json!({
                    "operation":"list_specialists_result",
                    "specialists":specialists,
                }))
            }
            ToolRequest::AssignSpecialistTask {
                target,
                objective,
                context_refs,
                expected_output,
                execution_intent,
                blocking: _,
                deadline_seconds,
            } => {
                let run_id = self.store.resolve_target(context.session_id, &target)?;
                let task = self.store.assign_task(
                    context,
                    &AssignSpecialistTask {
                        specialist_run_id: run_id,
                        objective,
                        context_refs,
                        expected_output,
                        requested_access: execution_intent,
                        deadline_seconds,
                    },
                    self.adapter,
                )?;
                Ok(serde_json::json!({
                    "operation":"assign_specialist_task_result",
                    "task_id":task.task_id,
                    "target_run_id":task.specialist_run_id,
                    "state":"accepted",
                }))
            }
            ToolRequest::AwaitSpecialistTasks {
                task_ids,
                return_when,
                transport_wait_seconds,
            } => {
                validate_wait(&return_when, transport_wait_seconds)?;
                let tasks = self.store.await_tasks(context.session_id, &task_ids)?;
                let pending = tasks
                    .iter()
                    .filter(|task| !task_terminal(&task.state))
                    .map(|task| task.task_id)
                    .collect::<Vec<_>>();
                Ok(serde_json::json!({
                    "operation":"await_specialist_tasks_result",
                    "tasks":tasks.iter().map(task_summary).collect::<Vec<_>>(),
                    "pending":pending,
                }))
            }
            ToolRequest::CollectSpecialistResults { limit } => {
                let tasks = self.store.collect_results(context.session_id, 0, limit)?;
                Ok(serde_json::json!({
                    "operation":"collect_specialist_results_result",
                    "tasks":tasks.iter().map(task_summary).collect::<Vec<_>>(),
                }))
            }
            ToolRequest::CancelSpecialistTask { task_id, reason } => {
                checked(&reason, 1_024, "reason")?;
                let prior = self.store.task(task_id)?;
                let was_terminal = task_terminal(&prior.state);
                let task = self
                    .store
                    .cancel_task(context.session_id, task_id, self.adapter)?;
                Ok(serde_json::json!({
                    "operation":"cancel_specialist_task_result",
                    "task_id":task.task_id,
                    "state":if was_terminal { "already_terminal" } else if task.state == "interrupted_unknown" { "interrupted_unknown" } else { "cancelled" },
                }))
            }
            ToolRequest::ReleaseSpecialist { run_id, reason } => {
                checked(&reason, 1_024, "reason")?;
                let member =
                    self.store
                        .release_specialist(context.session_id, run_id, self.adapter)?;
                Ok(serde_json::json!({
                    "operation":"release_specialist_result",
                    "run_id":member.run_id,
                    "state":member.membership_state,
                }))
            }
        }
    }
}

pub struct OrchestrationStore {
    connection: Connection,
    faults: Arc<dyn OrchestrationFaultInjector>,
    credential_root: PathBuf,
    uid: u32,
}

impl OrchestrationStore {
    pub fn open(state_root: &Path) -> Result<Self, MachineError> {
        Self::open_with_faults(state_root, Arc::new(NoOrchestrationFaults))
    }

    pub fn open_with_faults(
        state_root: &Path,
        faults: Arc<dyn OrchestrationFaultInjector>,
    ) -> Result<Self, MachineError> {
        let path = EngagementStore::workspace_database_path(state_root);
        let _ = EngagementStore::open(&path)?;
        let connection = Connection::open(&path).map_err(internal)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(internal)?;
        connection.execute_batch(SCHEMA).map_err(internal)?;
        Ok(Self {
            connection,
            faults,
            credential_root: state_root.join("orchestration/broker-credentials"),
            uid: crate::darwin::DarwinSystem.current_uid(),
        })
    }

    pub fn prepare_session(
        &mut self,
        workspace_id: &str,
        root_run_id: Uuid,
        idempotency_key: &str,
        request_sha256: &str,
        policy: &InstalledSpecialistPolicy,
    ) -> Result<OrchestratedSessionSnapshot, MachineError> {
        checked(workspace_id, 256, "workspace_id")?;
        checked(idempotency_key, 256, "idempotency_key")?;
        digest(request_sha256, "request_sha256")?;
        validate_installed_policy(policy)?;
        let policy_sha256 = policy.digest()?;
        if let Some((session, request)) = self
            .connection
            .query_row(
                "SELECT session_id,request_sha256 FROM aggregate_bootstrap_operations
                 WHERE workspace_id=?1 AND idempotency_key=?2",
                params![workspace_id, idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(internal)?
        {
            if session != root_run_id.to_string() || request != request_sha256 {
                return Err(idempotency_conflict(idempotency_key));
            }
            return self.session(root_run_id);
        }
        self.faults
            .check(OrchestrationBarrier::BeforeSessionCommit)?;
        let operation_id = new_uuid_v7();
        let now = now_ms()?;
        let policy_json = canonical_string(policy)?;
        let transaction = self.transaction()?;
        let external_membership: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM members WHERE specialist_run_id=?1 AND state!='retired'",
                [root_run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)?;
        let brokered_membership: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM brokered_members WHERE run_id=?1 AND membership_state!='retired'",
                [root_run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if external_membership != 0 || brokered_membership != 0 {
            return Err(session_conflict(
                root_run_id,
                "Run already belongs to another aggregate",
            ));
        }
        transaction
            .execute(
                "INSERT INTO orchestrated_sessions(
                   session_id,bootstrap_operation_id,root_run_id,workspace_id,status,
                   approval_policy,policy_json,policy_sha256,revision,created_at_ms,updated_at_ms
                 ) VALUES(?1,?2,?1,?3,'creating',?4,?5,?6,1,?7,?7)",
                params![
                    root_run_id.to_string(),
                    operation_id.to_string(),
                    workspace_id,
                    policy.approval_policy,
                    policy_json,
                    policy_sha256,
                    now
                ],
            )
            .map_err(internal)?;
        transaction
            .execute(
                "INSERT INTO aggregate_bootstrap_operations(
                   operation_id,session_id,workspace_id,idempotency_key,request_sha256,state,
                   safe_error_code,created_at_ms,updated_at_ms
                 ) VALUES(?1,?2,?3,?4,?5,'requested',NULL,?6,?6)",
                params![
                    operation_id.to_string(),
                    root_run_id.to_string(),
                    workspace_id,
                    idempotency_key,
                    request_sha256,
                    now
                ],
            )
            .map_err(internal)?;
        append_event(
            &transaction,
            root_run_id,
            "session_prepared",
            request_sha256,
            now,
        )?;
        transaction.commit().map_err(internal)?;
        self.faults
            .check(OrchestrationBarrier::AfterSessionCommit)?;
        self.session(root_run_id)
    }

    pub fn mark_primary_intent(&mut self, session_id: Uuid) -> Result<(), MachineError> {
        self.faults
            .check(OrchestrationBarrier::BeforePrimaryIntent)?;
        let now = now_ms()?;
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE aggregate_bootstrap_operations SET state='provisioning',updated_at_ms=?2
                 WHERE session_id=?1 AND state='requested'",
                params![session_id.to_string(), now],
            )
            .map_err(internal)?;
        if changed == 0 {
            let state: String = transaction
                .query_row(
                    "SELECT state FROM aggregate_bootstrap_operations WHERE session_id=?1",
                    [session_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(|_| session_not_found(session_id))?;
            if state != "provisioning" && state != "ready" {
                return Err(session_conflict(
                    session_id,
                    "bootstrap is not intent-writable",
                ));
            }
        } else {
            append_event(
                &transaction,
                session_id,
                "primary_publication_started",
                &sha256_hex(b"provisioning"),
                now,
            )?;
        }
        transaction.commit().map_err(internal)?;
        self.faults.check(OrchestrationBarrier::AfterPrimaryIntent)
    }

    pub fn finish_primary_publication(
        &mut self,
        session_id: Uuid,
        published: Result<(), AdapterFailure>,
    ) -> Result<OrchestratedSessionSnapshot, MachineError> {
        self.faults
            .check(OrchestrationBarrier::BeforePrimaryPublication)?;
        let (operation_state, session_state, error) = match published {
            Ok(()) => ("ready", "active", None),
            Err(AdapterFailure::Rejected(code)) => ("failed", "aborted", Some(code)),
            Err(AdapterFailure::Unknown) => (
                "recovery_required",
                "recovering",
                Some("PRIMARY_PUBLICATION_UNKNOWN".to_owned()),
            ),
        };
        let current: (String, String) = self
            .connection
            .query_row(
                "SELECT b.state,s.status FROM aggregate_bootstrap_operations b
                 JOIN orchestrated_sessions s USING(session_id) WHERE b.session_id=?1",
                [session_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| session_not_found(session_id))?;
        if current.0 == operation_state && current.1 == session_state {
            return self.session(session_id);
        }
        if !matches!(
            current.0.as_str(),
            "requested" | "provisioning" | "recovery_required"
        ) || !matches!(current.1.as_str(), "creating" | "recovering")
        {
            return Err(session_conflict(
                session_id,
                "Primary publication already has a different authoritative outcome",
            ));
        }
        let now = now_ms()?;
        let transaction = self.transaction()?;
        let operation_changed = transaction
            .execute(
                "UPDATE aggregate_bootstrap_operations SET state=?2,safe_error_code=?3,updated_at_ms=?4
                 WHERE session_id=?1 AND state IN ('requested','provisioning','recovery_required')",
                params![session_id.to_string(), operation_state, error, now],
            )
            .map_err(internal)?;
        let session_changed = transaction
            .execute(
                "UPDATE orchestrated_sessions SET status=?2,revision=revision+1,updated_at_ms=?3
                 WHERE session_id=?1 AND status IN ('creating','recovering')",
                params![session_id.to_string(), session_state, now],
            )
            .map_err(internal)?;
        if operation_changed != 1 || session_changed != 1 {
            return Err(session_conflict(
                session_id,
                "Primary publication changed concurrently",
            ));
        }
        append_event(
            &transaction,
            session_id,
            "primary_publication_settled",
            &sha256_hex(operation_state.as_bytes()),
            now,
        )?;
        transaction.commit().map_err(internal)?;
        self.faults
            .check(OrchestrationBarrier::AfterPrimaryPublication)?;
        self.session(session_id)
    }

    pub fn session(&self, session_id: Uuid) -> Result<OrchestratedSessionSnapshot, MachineError> {
        let row = self
            .connection
            .query_row(
                "SELECT bootstrap_operation_id,root_run_id,workspace_id,status,approval_policy,
                        policy_json,policy_sha256,revision
                 FROM orchestrated_sessions WHERE session_id=?1",
                [session_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, u64>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(|| session_not_found(session_id))?;
        let bootstrap_operation_id = parse_uuid(&row.0)?;
        let root_run_id = parse_uuid(&row.1)?;
        if root_run_id != session_id
            || !matches!(
                row.3.as_str(),
                "creating"
                    | "active"
                    | "degraded"
                    | "recovering"
                    | "completing"
                    | "aborting"
                    | "completed"
                    | "aborted"
            )
            || !matches!(row.4.as_str(), "user_approval_required" | "fully_delegated")
            || row.7 == 0
        {
            return Err(integrity("session identity or lifecycle is invalid"));
        }
        let policy: InstalledSpecialistPolicy = serde_json::from_str(&row.5).map_err(internal)?;
        validate_installed_policy(&policy)
            .map_err(|_| integrity("session policy snapshot contract is invalid"))?;
        if policy.digest()? != row.6 || policy.approval_policy != row.4 {
            return Err(integrity("session policy snapshot digest is invalid"));
        }
        let members = self.members(session_id)?;
        for member in &members {
            let role = policy
                .role(&member.role_ref)
                .map_err(|_| integrity("member role is absent from the session policy"))?;
            if member.parent_run_id != root_run_id
                || &member.role_snapshot != role
                || member.role_snapshot_sha256 != digest_value(role)?
                || member.agent_configuration != role.agent_configuration
                || member.agent_configuration_sha256
                    != crate::run::agent_configuration_digest(&role.agent_configuration)
                        .map_err(internal)?
            {
                return Err(integrity(
                    "member snapshot disagrees with the immutable session policy",
                ));
            }
        }
        self.validate_event_chain(session_id)?;
        Ok(OrchestratedSessionSnapshot {
            session_id,
            bootstrap_operation_id,
            root_run_id,
            workspace_id: row.2,
            status: row.3,
            composition_state: if members
                .iter()
                .any(|member| member.membership_state != "retired")
            {
                "brokered_hierarchy"
            } else {
                "standalone_primary"
            }
            .to_owned(),
            approval_policy: row.4,
            specialist_policy: policy,
            specialist_policy_sha256: row.6,
            revision: row.7,
            members,
        })
    }

    pub fn request_specialist<A: OrchestrationAdapter>(
        &mut self,
        context: &PrimaryCallContext,
        request: &RequestSpecialist,
        adapter: &mut A,
    ) -> Result<SpecialistOperationSnapshot, MachineError> {
        validate_context(context)?;
        validate_request_specialist(request)?;
        let session = self.authorize_primary(context)?;
        let role = session.specialist_policy.role(&request.role_ref)?.clone();
        if !role.primary_may_request || !role.allowed_access.contains(&request.requested_access) {
            return Err(policy_denied(
                &request.role_ref,
                "Role or requested access is not allowed",
            ));
        }
        let request_sha256 = digest_value(request)?;
        if let Some(existing) =
            self.load_spawn_by_key(context.session_id, &context.idempotency_key)?
        {
            if self.spawn_request_digest(existing.operation_id)? != request_sha256 {
                return Err(idempotency_conflict(&context.idempotency_key));
            }
            if existing.state == "requested" {
                let run_id = existing
                    .specialist_run_id
                    .ok_or_else(|| integrity("reserved Specialist identity is missing"))?;
                let credential = BrokerCredential::load(&self.credential_root, self.uid, run_id)?;
                return self.publish_reserved_specialist(
                    context,
                    &role,
                    existing.operation_id,
                    run_id,
                    &credential,
                    adapter,
                );
            }
            if existing.state == "awaiting_approval" {
                let approval_request_id = existing
                    .approval_request_id
                    .ok_or_else(|| integrity("approval request identity is missing"))?;
                approval_result(adapter.publish_approval_request(
                    context.session_id,
                    existing.operation_id,
                    approval_request_id,
                    request,
                ))?;
            }
            return Ok(existing);
        }
        if let Some(existing) = self.load_reuse_receipt(
            context.session_id,
            &context.idempotency_key,
            &request_sha256,
        )? {
            return Ok(existing);
        }
        let role_snapshot_sha256 = digest_value(&role)?;
        let agent_configuration_sha256 =
            crate::run::agent_configuration_digest(&role.agent_configuration).map_err(internal)?;
        let mut reusable = session
            .members
            .iter()
            .filter(|member| {
                role.reuse_policy != "never"
                    && member.role_ref == request.role_ref
                    && member.role_snapshot_sha256 == role_snapshot_sha256
                    && member.agent_configuration_sha256 == agent_configuration_sha256
                    && member.requested_access == request.requested_access
                    && member.membership_state == "active"
            })
            .map(|member| {
                Ok((
                    self.member_pending_count(member.run_id)?,
                    member.run_id,
                    member,
                ))
            })
            .collect::<Result<Vec<_>, MachineError>>()?;
        if role.reuse_policy == "reuse_idle_compatible" {
            reusable.retain(|(pending, _, _)| *pending == 0);
        }
        reusable.sort_by_key(|(pending, run_id, _)| (*pending != 0, *pending, *run_id));
        if let Some((_, _, member)) = reusable.first() {
            return self.record_reuse_receipt(
                context.session_id,
                &context.idempotency_key,
                &request_sha256,
                member.spawn_operation_id,
            );
        }
        let active_total = session
            .members
            .iter()
            .filter(|member| member.membership_state != "retired")
            .count();
        let active_role = session
            .members
            .iter()
            .filter(|member| {
                member.membership_state != "retired" && member.role_ref == request.role_ref
            })
            .count();
        if active_total >= session.specialist_policy.max_active_specialists as usize
            || active_role >= role.max_active_instances as usize
        {
            return Err(policy_denied(
                &request.role_ref,
                "active Specialist limit is reached",
            ));
        }
        let automatic = session.approval_policy == "fully_delegated";
        if automatic && !role.auto_approve_when_fully_delegated {
            return Err(policy_denied(
                &request.role_ref,
                "Role is not eligible for fully delegated approval",
            ));
        }
        let operation_id = new_uuid_v7();
        let approval_request_id = (!automatic).then(new_uuid_v7);
        if !automatic {
            self.insert_spawn(
                context,
                request,
                operation_id,
                None,
                approval_request_id,
                "awaiting_approval",
                &request_sha256,
                None,
                None,
            )?;
            approval_result(adapter.publish_approval_request(
                context.session_id,
                operation_id,
                approval_request_id.expect("nonautomatic request has an approval identity"),
                request,
            ))?;
            return self.spawn(operation_id);
        }
        self.provision_specialist(
            context,
            request,
            &role,
            operation_id,
            None,
            &request_sha256,
            adapter,
        )
    }

    pub fn decide_specialist<A: OrchestrationAdapter>(
        &mut self,
        context: &PrimaryCallContext,
        operation_id: Uuid,
        approved: bool,
        adapter: &mut A,
    ) -> Result<SpecialistOperationSnapshot, MachineError> {
        validate_context(context)?;
        let session = self.authorize_primary(context)?;
        let operation = self.spawn(operation_id)?;
        if self.spawn_session(operation_id)? != context.session_id {
            return Err(specialist_not_member(operation_id));
        }
        if operation.approval_request_id.is_none() {
            return Err(MachineError::invalid_argument(
                "operation_id",
                "the Specialist operation has no user approval decision",
            ));
        }
        if operation.state == "denied" {
            return if approved {
                Err(idempotency_conflict(&operation_id.to_string()))
            } else {
                Ok(operation)
            };
        }
        if operation.state != "awaiting_approval" && !approved {
            return Err(idempotency_conflict(&operation_id.to_string()));
        }
        if operation.state == "requested" {
            let run_id = operation
                .specialist_run_id
                .ok_or_else(|| integrity("reserved Specialist identity is missing"))?;
            let credential = BrokerCredential::load(&self.credential_root, self.uid, run_id)?;
            let role = session.specialist_policy.role(&operation.role_ref)?.clone();
            return self.publish_reserved_specialist(
                context,
                &role,
                operation_id,
                run_id,
                &credential,
                adapter,
            );
        }
        if operation.state != "awaiting_approval" {
            return Ok(operation);
        }
        if !approved {
            let now = now_ms()?;
            let transaction = self.transaction()?;
            transaction
                .execute(
                    "UPDATE brokered_spawn_operations SET state='denied',
                     safe_error_code='USER_DENIED',updated_at_ms=?2
                     WHERE operation_id=?1 AND state='awaiting_approval'",
                    params![operation_id.to_string(), now],
                )
                .map_err(internal)?;
            append_event(
                &transaction,
                context.session_id,
                "specialist_request_denied",
                &sha256_hex(operation_id.as_bytes()),
                now,
            )?;
            transaction.commit().map_err(internal)?;
            return self.spawn(operation_id);
        }
        let (request_json, request_sha256): (String, String) = self
            .connection
            .query_row(
                "SELECT request_json,request_sha256 FROM brokered_spawn_operations
                 WHERE operation_id=?1 AND session_id=?2",
                params![operation_id.to_string(), context.session_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(internal)?;
        let request: RequestSpecialist = serde_json::from_str(&request_json).map_err(internal)?;
        let role = session.specialist_policy.role(&request.role_ref)?.clone();
        self.provision_specialist(
            context,
            &request,
            &role,
            operation_id,
            operation.approval_request_id,
            &request_sha256,
            adapter,
        )
    }

    pub fn specialist_operations(
        &self,
        session_id: Uuid,
        operation_ids: &[Uuid],
    ) -> Result<Vec<SpecialistOperationSnapshot>, MachineError> {
        if operation_ids.is_empty() || operation_ids.len() > 16 {
            return Err(MachineError::invalid_argument(
                "operation_ids",
                "one to 16 operation identities are required",
            ));
        }
        operation_ids
            .iter()
            .map(|operation_id| {
                let operation = self.spawn(*operation_id)?;
                if self.spawn_session(*operation_id)? != session_id {
                    return Err(specialist_not_member(*operation_id));
                }
                Ok(operation)
            })
            .collect()
    }

    pub fn members(&self, session_id: Uuid) -> Result<Vec<BrokeredMemberSnapshot>, MachineError> {
        let policy_json: String = self
            .connection
            .query_row(
                "SELECT policy_json FROM orchestrated_sessions WHERE session_id=?1",
                [session_id.to_string()],
                |row| row.get(0),
            )
            .map_err(|_| session_not_found(session_id))?;
        let policy: InstalledSpecialistPolicy = serde_json::from_str(&policy_json)
            .map_err(|_| integrity("session policy snapshot is invalid"))?;
        validate_installed_policy(&policy)
            .map_err(|_| integrity("session policy snapshot contract is invalid"))?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT run_id,parent_run_id,role_ref,role_snapshot_sha256,
                        agent_configuration_json,agent_configuration_sha256,controller_binding_json,
                        spawn_operation_id,spawned_by_turn_id,membership_state,actor_residency,
                        activation_policy,requested_access
                 FROM brokered_members WHERE session_id=?1 ORDER BY created_at_ms,run_id",
            )
            .map_err(internal)?;
        let rows = statement
            .query_map([session_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                ))
            })
            .map_err(internal)?;
        rows.map(|row| {
            let row = row.map_err(internal)?;
            let configuration: crate::run::AgentConfigurationSnapshot =
                serde_json::from_str(&row.4)
                    .map_err(|_| integrity("member Agent Configuration is invalid"))?;
            let controller: ControllerBinding = serde_json::from_str(&row.6)
                .map_err(|_| integrity("member Controller binding is invalid"))?;
            if crate::run::agent_configuration_digest(&configuration).map_err(internal)? != row.5
                || configuration.role_reference.as_deref() != Some(row.2.as_str())
                || controller.identity.kind != ControllerKind::Automation
                || controller.identity.generation != 1
            {
                return Err(integrity("member authority snapshot is invalid"));
            }
            digest(
                &controller.capability_sha256,
                "controller capability digest",
            )?;
            let role_snapshot = policy
                .role(&row.2)
                .map_err(|_| integrity("member role is absent from the session policy"))?
                .clone();
            Ok(BrokeredMemberSnapshot {
                run_id: parse_uuid(&row.0)?,
                parent_run_id: parse_uuid(&row.1)?,
                role_ref: row.2,
                role_snapshot,
                role_snapshot_sha256: row.3,
                agent_configuration: configuration,
                agent_configuration_sha256: row.5,
                controller_binding: controller,
                spawn_operation_id: parse_uuid(&row.7)?,
                spawned_by_turn_id: row.8,
                membership_state: row.9,
                actor_residency: row.10,
                activation_policy: row.11,
                requested_access: row.12,
            })
        })
        .collect()
    }

    fn validate_event_chain(&self, session_id: Uuid) -> Result<(), MachineError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT kind,payload_sha256,previous_hash,event_hash,created_at_ms
                 FROM orchestration_events WHERE session_id=?1 ORDER BY sequence",
            )
            .map_err(internal)?;
        let rows = statement
            .query_map([session_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(internal)?;
        let mut previous = "0".repeat(64);
        let mut observed = false;
        for row in rows {
            let (kind, payload, stored_previous, event_hash, created_at_ms) =
                row.map_err(internal)?;
            digest(&payload, "event payload digest")?;
            digest(&stored_previous, "event previous hash")?;
            digest(&event_hash, "event hash")?;
            checked(&kind, 128, "event kind")?;
            let expected = sha256_hex(
                format!("{previous}\0{session_id}\0{kind}\0{payload}\0{created_at_ms}").as_bytes(),
            );
            if stored_previous != previous || event_hash != expected {
                return Err(integrity("orchestration event chain is invalid"));
            }
            previous = event_hash;
            observed = true;
        }
        if !observed {
            return Err(integrity("orchestration event chain is empty"));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn provision_specialist<A: OrchestrationAdapter>(
        &mut self,
        context: &PrimaryCallContext,
        request: &RequestSpecialist,
        role: &InstalledSpecialistRole,
        operation_id: Uuid,
        approval_request_id: Option<Uuid>,
        request_sha256: &str,
        adapter: &mut A,
    ) -> Result<SpecialistOperationSnapshot, MachineError> {
        let run_id = new_uuid_v7();
        self.faults
            .check(OrchestrationBarrier::BeforeSpawnReservation)?;
        let credential = BrokerCredential::create(&self.credential_root, self.uid, run_id)?;
        let reserved = if approval_request_id.is_some() {
            let now = now_ms()?;
            let transaction = self.transaction()?;
            let changed = transaction
                .execute(
                    "UPDATE brokered_spawn_operations SET child_run_id=?2,state='requested',
                     updated_at_ms=?3 WHERE operation_id=?1 AND state='awaiting_approval'",
                    params![operation_id.to_string(), run_id.to_string(), now],
                )
                .map_err(internal)?;
            if changed != 1 {
                return Err(session_conflict(
                    context.session_id,
                    "approval operation changed before provisioning",
                ));
            }
            insert_member(
                &transaction,
                context,
                request,
                role,
                operation_id,
                run_id,
                &credential.binding,
                now,
            )?;
            append_event(
                &transaction,
                context.session_id,
                "specialist_approved",
                request_sha256,
                now,
            )?;
            transaction.commit().map_err(internal)?;
            Ok(())
        } else {
            self.insert_spawn(
                context,
                request,
                operation_id,
                Some(run_id),
                None,
                "requested",
                request_sha256,
                Some(role),
                Some(&credential.binding),
            )
        };
        if let Err(error) = reserved {
            let _ = credential.remove();
            return Err(error);
        }
        self.faults
            .check(OrchestrationBarrier::AfterSpawnReservation)?;
        self.publish_reserved_specialist(context, role, operation_id, run_id, &credential, adapter)
    }

    fn publish_reserved_specialist<A: OrchestrationAdapter>(
        &mut self,
        context: &PrimaryCallContext,
        role: &InstalledSpecialistRole,
        operation_id: Uuid,
        run_id: Uuid,
        credential: &BrokerCredential,
        adapter: &mut A,
    ) -> Result<SpecialistOperationSnapshot, MachineError> {
        let now = now_ms()?;
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE brokered_spawn_operations SET state='provisioning',updated_at_ms=?2
                 WHERE operation_id=?1 AND state='requested'",
                params![operation_id.to_string(), now],
            )
            .map_err(internal)?;
        if changed != 1 {
            return Err(session_conflict(
                context.session_id,
                "reserved Specialist is not publishable",
            ));
        }
        append_event(
            &transaction,
            context.session_id,
            "specialist_publication_started",
            &sha256_hex(operation_id.as_bytes()),
            now,
        )?;
        transaction.commit().map_err(internal)?;
        let agent_configuration_sha256 =
            crate::run::agent_configuration_digest(&role.agent_configuration).map_err(internal)?;
        let member = self.member(context.session_id, run_id)?;
        validate_member_credential(&member, credential)?;
        let plan = BrokeredRunPlan {
            session_id: context.session_id,
            run_id,
            aggregate_binding: AggregateBinding {
                aggregate_kind: crate::domain::AggregateKind::OrchestratedSession,
                aggregate_id: context.session_id,
                member_kind: AggregateMemberKind::Specialist,
                operation_id,
                policy_sha256: None,
                role_reference: Some(role.role_ref.clone()),
                role_snapshot_sha256: Some(digest_value(role)?),
                agent_configuration_sha256: Some(agent_configuration_sha256),
            },
            role: role.clone(),
            requested_access: member.requested_access,
        };
        self.faults
            .check(OrchestrationBarrier::BeforeWorkerPublication)?;
        let published = adapter.publish_specialist(&plan, credential);
        self.faults
            .check(OrchestrationBarrier::AfterWorkerPublication)?;
        let outcome = match published {
            Ok(()) => {
                self.faults
                    .check(OrchestrationBarrier::BeforeThreadCreation)?;
                let result = adapter.create_thread(&plan, credential);
                self.faults
                    .check(OrchestrationBarrier::AfterThreadCreation)?;
                result
            }
            failure => failure,
        };
        let (state, member_state, residency, error) = match outcome {
            Ok(()) => ("ready", "active", "resident", None),
            Err(AdapterFailure::Rejected(code)) => ("failed", "retired", "terminal", Some(code)),
            Err(AdapterFailure::Unknown) => (
                "recovery_required",
                "degraded",
                "unavailable",
                Some("SPECIALIST_PUBLICATION_UNKNOWN".to_owned()),
            ),
        };
        let now = now_ms()?;
        let transaction = self.transaction()?;
        transaction
            .execute(
                "UPDATE brokered_spawn_operations SET state=?2,safe_error_code=?3,updated_at_ms=?4
                 WHERE operation_id=?1",
                params![operation_id.to_string(), state, error, now],
            )
            .map_err(internal)?;
        transaction
            .execute(
                "UPDATE brokered_members SET membership_state=?2,actor_residency=?3,updated_at_ms=?4
                 WHERE run_id=?1",
                params![run_id.to_string(), member_state, residency, now],
            )
            .map_err(internal)?;
        transaction
            .execute(
                "UPDATE orchestrated_sessions SET revision=revision+1,updated_at_ms=?2 WHERE session_id=?1",
                params![context.session_id.to_string(), now],
            )
            .map_err(internal)?;
        append_event(
            &transaction,
            context.session_id,
            "specialist_provisioning_settled",
            &sha256_hex(state.as_bytes()),
            now,
        )?;
        transaction.commit().map_err(internal)?;
        if state == "failed" {
            let credential = BrokerCredential::load(&self.credential_root, self.uid, run_id)?;
            let _ = credential.remove();
        }
        self.spawn(operation_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_spawn(
        &mut self,
        context: &PrimaryCallContext,
        request: &RequestSpecialist,
        operation_id: Uuid,
        child_run_id: Option<Uuid>,
        approval_request_id: Option<Uuid>,
        state: &str,
        request_sha256: &str,
        role: Option<&InstalledSpecialistRole>,
        controller: Option<&ControllerBinding>,
    ) -> Result<(), MachineError> {
        let now = now_ms()?;
        let transaction = self.transaction()?;
        transaction
            .execute(
                "INSERT INTO brokered_spawn_operations(
                   operation_id,session_id,idempotency_key,request_sha256,request_json,role_ref,
                   parent_run_id,child_run_id,approval_request_id,state,safe_error_code,
                   source_turn_id,source_tool_call_id,created_at_ms,updated_at_ms
                 ) VALUES(?1,?2,?3,?4,?5,?6,?2,?7,?8,?9,NULL,?10,?11,?12,?12)",
                params![
                    operation_id.to_string(),
                    context.session_id.to_string(),
                    context.idempotency_key,
                    request_sha256,
                    canonical_string(request)?,
                    request.role_ref,
                    child_run_id.map(|id| id.to_string()),
                    approval_request_id.map(|id| id.to_string()),
                    state,
                    context.source_turn_id,
                    context.source_tool_call_id,
                    now
                ],
            )
            .map_err(internal)?;
        if let (Some(run_id), Some(role), Some(controller)) = (child_run_id, role, controller) {
            insert_member(
                &transaction,
                context,
                request,
                role,
                operation_id,
                run_id,
                controller,
                now,
            )?;
        }
        append_event(
            &transaction,
            context.session_id,
            "specialist_requested",
            request_sha256,
            now,
        )?;
        transaction.commit().map_err(internal)
    }

    pub fn assign_task<A: OrchestrationAdapter>(
        &mut self,
        context: &PrimaryCallContext,
        request: &AssignSpecialistTask,
        adapter: &mut A,
    ) -> Result<SpecialistTaskSnapshot, MachineError> {
        validate_context(context)?;
        validate_assign_task(request)?;
        let _session = self.authorize_primary(context)?;
        let member = self.member(context.session_id, request.specialist_run_id)?;
        if member.membership_state != "active" {
            return Err(specialist_not_member(request.specialist_run_id));
        }
        let credential = BrokerCredential::load(&self.credential_root, self.uid, member.run_id)?;
        validate_member_credential(&member, &credential)?;
        let request_sha256 = digest_value(request)?;
        let task_id = if let Some(existing) =
            self.task_by_key(context.session_id, &context.idempotency_key)?
        {
            if self.task_request_digest(existing.task_id)? != request_sha256 {
                return Err(idempotency_conflict(&context.idempotency_key));
            }
            match existing.state.as_str() {
                "accepted" => existing.task_id,
                "dispatching" | "running" | "interrupted_unknown" => {
                    return Err(interrupted_unknown(existing.task_id));
                }
                "failed" => {
                    return Err(task_failed(
                        existing.task_id,
                        existing
                            .safe_error_code
                            .as_deref()
                            .unwrap_or("SPECIALIST_REQUEST_DENIED"),
                    ));
                }
                _ => return Ok(existing),
            }
        } else {
            let task_id = new_uuid_v7();
            let now = now_ms()?;
            let transaction = self.transaction()?;
            transaction
                .execute(
                    "INSERT INTO brokered_tasks(
                   task_id,session_id,idempotency_key,request_sha256,source_run_id,source_turn_id,
                   source_tool_call_id,target_run_id,request_json,target_turn_id,result_json,
                   result_sha256,result_artifact_id,state,safe_error_code,created_at_ms,updated_at_ms
                 ) VALUES(?1,?2,?3,?4,?2,?5,?6,?7,?8,NULL,NULL,NULL,NULL,'accepted',NULL,?9,?9)",
                    params![
                        task_id.to_string(),
                        context.session_id.to_string(),
                        context.idempotency_key,
                        request_sha256,
                        context.source_turn_id,
                        context.source_tool_call_id,
                        request.specialist_run_id.to_string(),
                        canonical_string(request)?,
                        now
                    ],
                )
                .map_err(|error| {
                    if error
                        .to_string()
                        .contains("one_active_brokered_task_per_member")
                    {
                        session_conflict(context.session_id, "Specialist already has active work")
                    } else {
                        internal(error)
                    }
                })?;
            append_event(
                &transaction,
                context.session_id,
                "specialist_task_accepted",
                &request_sha256,
                now,
            )?;
            transaction.commit().map_err(internal)?;
            task_id
        };
        if request.requested_access == "canonical_workspace_write" {
            let writer = adapter
                .release_writer(context.source_run_id)
                .and_then(|()| adapter.verify_writer_none())
                .and_then(|()| adapter.acquire_writer(request.specialist_run_id));
            if let Err(failure) = writer {
                let (state, code) = match failure {
                    AdapterFailure::Rejected(_) => ("failed", "SPECIALIST_WRITER_CONFLICT"),
                    AdapterFailure::Unknown => ("interrupted_unknown", "INTERRUPTED_UNKNOWN"),
                };
                self.settle_task_error(context.session_id, task_id, state, code)?;
                return Err(if state == "interrupted_unknown" {
                    interrupted_unknown(task_id)
                } else {
                    task_failed(task_id, code)
                });
            }
        }
        self.faults
            .check(OrchestrationBarrier::BeforeTaskDispatch)?;
        let now = now_ms()?;
        let transaction = self.transaction()?;
        transaction
            .execute(
                "UPDATE brokered_tasks SET state='dispatching',updated_at_ms=?2 WHERE task_id=?1",
                params![task_id.to_string(), now],
            )
            .map_err(internal)?;
        append_event(
            &transaction,
            context.session_id,
            "specialist_task_dispatch_started",
            &sha256_hex(task_id.as_bytes()),
            now,
        )?;
        transaction.commit().map_err(internal)?;
        let outcome = adapter.dispatch_task(&member, request, &credential);
        self.faults.check(OrchestrationBarrier::AfterTaskDispatch)?;
        self.faults
            .check(OrchestrationBarrier::BeforeResultAppend)?;
        let now = now_ms()?;
        let transaction = self.transaction()?;
        let terminal_error = match outcome {
            Ok(completed) => {
                let result_json = canonical_string(&completed.result)?;
                let result_sha256 = sha256_hex(result_json.as_bytes());
                let artifact_id = new_uuid_v7();
                transaction
                    .execute(
                        "UPDATE brokered_tasks SET state='completed_not_delivered',target_turn_id=?2,
                         result_json=?3,result_sha256=?4,result_artifact_id=?5,updated_at_ms=?6
                         WHERE task_id=?1",
                        params![
                            task_id.to_string(),
                            completed.turn_id,
                            result_json,
                            result_sha256,
                            artifact_id.to_string(),
                            now
                        ],
                    )
                    .map_err(internal)?;
                None
            }
            Err(AdapterFailure::Rejected(code)) => {
                transaction
                    .execute(
                        "UPDATE brokered_tasks SET state='failed',safe_error_code=?2,updated_at_ms=?3
                         WHERE task_id=?1",
                        params![task_id.to_string(), code, now],
                    )
                    .map_err(internal)?;
                Some(task_failed(task_id, &code))
            }
            Err(AdapterFailure::Unknown) => {
                transaction
                    .execute(
                        "UPDATE brokered_tasks SET state='interrupted_unknown',
                         safe_error_code='TARGET_TURN_OUTCOME_UNKNOWN',updated_at_ms=?2 WHERE task_id=?1",
                        params![task_id.to_string(), now],
                    )
                    .map_err(internal)?;
                Some(interrupted_unknown(task_id))
            }
        };
        let settled = task_state_from_transaction(&transaction, task_id)?;
        append_event(
            &transaction,
            context.session_id,
            "specialist_task_settled",
            &sha256_hex(settled.as_bytes()),
            now,
        )?;
        transaction.commit().map_err(internal)?;
        self.faults.check(OrchestrationBarrier::AfterResultAppend)?;
        match terminal_error {
            Some(error) => Err(error),
            None => self.task(task_id),
        }
    }

    fn settle_task_error(
        &mut self,
        session_id: Uuid,
        task_id: Uuid,
        state: &str,
        code: &str,
    ) -> Result<(), MachineError> {
        let now = now_ms()?;
        let transaction = self.transaction()?;
        transaction
            .execute(
                "UPDATE brokered_tasks SET state=?2,safe_error_code=?3,updated_at_ms=?4
                 WHERE task_id=?1 AND state='accepted'",
                params![task_id.to_string(), state, code, now],
            )
            .map_err(internal)?;
        append_event(
            &transaction,
            session_id,
            "specialist_task_settled",
            &sha256_hex(state.as_bytes()),
            now,
        )?;
        transaction.commit().map_err(internal)
    }

    pub fn await_tasks(
        &self,
        session_id: Uuid,
        task_ids: &[Uuid],
    ) -> Result<Vec<SpecialistTaskSnapshot>, MachineError> {
        if task_ids.is_empty() || task_ids.len() > 64 {
            return Err(MachineError::invalid_argument(
                "task_ids",
                "one to 64 task identities are required",
            ));
        }
        task_ids
            .iter()
            .map(|task_id| {
                let task = self.task(*task_id)?;
                if self.task_session(*task_id)? != session_id {
                    return Err(specialist_task_not_found(*task_id));
                }
                Ok(task)
            })
            .collect()
    }

    pub fn collect_results(
        &mut self,
        session_id: Uuid,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<SpecialistTaskSnapshot>, MachineError> {
        if !(1..=64).contains(&limit) {
            return Err(MachineError::invalid_argument(
                "limit",
                "limit must be 1 to 64",
            ));
        }
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let already_delivered = {
            let mut statement = transaction
                .prepare(
                    "SELECT task_id FROM brokered_delivery_receipts r
                     JOIN brokered_tasks t USING(task_id)
                     WHERE t.session_id=?1 AND r.sequence>?2 ORDER BY r.sequence LIMIT ?3",
                )
                .map_err(internal)?;
            statement
                .query_map(
                    params![session_id.to_string(), after_sequence, limit as u64],
                    |row| row.get::<_, String>(0),
                )
                .map_err(internal)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(internal)?
        };
        let remaining = limit - already_delivered.len();
        let task_ids = if remaining == 0 {
            Vec::new()
        } else {
            let mut statement = transaction
                .prepare(
                    "SELECT task_id FROM brokered_tasks WHERE session_id=?1
                     AND state='completed_not_delivered' ORDER BY created_at_ms,task_id LIMIT ?2",
                )
                .map_err(internal)?;
            statement
                .query_map(params![session_id.to_string(), remaining as u64], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(internal)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(internal)?
        };
        if !task_ids.is_empty() {
            faults.check(OrchestrationBarrier::BeforeDeliveryReceipt)?;
        }
        for task_id in &task_ids {
            let now = now_ms()?;
            let inserted = transaction
                .execute(
                    "INSERT OR IGNORE INTO brokered_delivery_receipts(task_id,delivered_at_ms)
                     VALUES(?1,?2)",
                    params![task_id, now],
                )
                .map_err(internal)?;
            let changed = transaction
                .execute(
                    "UPDATE brokered_tasks SET state='delivered',updated_at_ms=?2
                     WHERE task_id=?1 AND state='completed_not_delivered'",
                    params![task_id, now],
                )
                .map_err(internal)?;
            if inserted != 1 || changed != 1 {
                return Err(integrity(
                    "result delivery receipt disagrees with the pending task",
                ));
            }
            append_event(
                &transaction,
                session_id,
                "specialist_result_delivered",
                &sha256_hex(task_id.as_bytes()),
                now,
            )?;
        }
        transaction.commit().map_err(internal)?;
        if !task_ids.is_empty() {
            faults.check(OrchestrationBarrier::AfterDeliveryReceipt)?;
        }
        let ids = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT task_id FROM brokered_delivery_receipts r
                     JOIN brokered_tasks t USING(task_id)
                     WHERE t.session_id=?1 AND r.sequence>?2 ORDER BY r.sequence LIMIT ?3",
                )
                .map_err(internal)?;
            statement
                .query_map(
                    params![session_id.to_string(), after_sequence, limit as u64],
                    |row| row.get::<_, String>(0),
                )
                .map_err(internal)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(internal)?
        };
        ids.into_iter()
            .map(|id| self.task(parse_uuid(&id)?))
            .collect()
    }

    pub fn cancel_task<A: OrchestrationAdapter>(
        &mut self,
        session_id: Uuid,
        task_id: Uuid,
        adapter: &mut A,
    ) -> Result<SpecialistTaskSnapshot, MachineError> {
        if self.task_session(task_id)? != session_id {
            return Err(specialist_task_not_found(task_id));
        }
        let task = self.task(task_id)?;
        if matches!(
            task.state.as_str(),
            "delivered" | "failed" | "interrupted_unknown" | "cancelled"
        ) {
            return Ok(task);
        }
        let member = self.member(session_id, task.specialist_run_id)?;
        let credential =
            BrokerCredential::load(&self.credential_root, self.uid, task.specialist_run_id)?;
        validate_member_credential(&member, &credential)?;
        let state = match adapter.cancel_task(&task, &credential) {
            Ok(()) => "cancelled",
            Err(AdapterFailure::Rejected(_)) if task.state == "accepted" => "cancelled",
            Err(AdapterFailure::Rejected(_)) | Err(AdapterFailure::Unknown) => {
                "interrupted_unknown"
            }
        };
        let now = now_ms()?;
        let transaction = self.transaction()?;
        transaction
            .execute(
                "UPDATE brokered_tasks SET state=?2,safe_error_code=?3,updated_at_ms=?4 WHERE task_id=?1",
                params![
                    task_id.to_string(),
                    state,
                    (state == "interrupted_unknown").then_some("CANCEL_OUTCOME_UNKNOWN"),
                    now
                ],
            )
            .map_err(internal)?;
        append_event(
            &transaction,
            session_id,
            "specialist_task_cancelled",
            &sha256_hex(state.as_bytes()),
            now,
        )?;
        transaction.commit().map_err(internal)?;
        self.task(task_id)
    }

    pub fn release_specialist<A: OrchestrationAdapter>(
        &mut self,
        session_id: Uuid,
        run_id: Uuid,
        adapter: &mut A,
    ) -> Result<BrokeredMemberSnapshot, MachineError> {
        let member = self.member(session_id, run_id)?;
        if member.membership_state == "retired" {
            return Ok(member);
        }
        let active: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM brokered_tasks WHERE target_run_id=?1
                 AND state IN ('accepted','dispatching','running','completed_not_delivered')",
                [run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if active != 0 {
            return Err(session_conflict(
                session_id,
                "Specialist has unfinished work",
            ));
        }
        let credential = BrokerCredential::load(&self.credential_root, self.uid, run_id)?;
        validate_member_credential(&member, &credential)?;
        adapter_result(adapter.release_writer(run_id))?;
        adapter_result(adapter.verify_writer_none())?;
        match adapter.release_specialist(&member, &credential) {
            Ok(()) => {
                let now = now_ms()?;
                let transaction = self.transaction()?;
                transaction
                    .execute(
                        "UPDATE brokered_members SET membership_state='retired',actor_residency='terminal',
                         updated_at_ms=?2 WHERE run_id=?1",
                        params![run_id.to_string(), now],
                    )
                    .map_err(internal)?;
                append_event(
                    &transaction,
                    session_id,
                    "specialist_released",
                    &sha256_hex(run_id.as_bytes()),
                    now,
                )?;
                transaction.commit().map_err(internal)?;
                credential.remove()?;
            }
            Err(AdapterFailure::Rejected(code)) => return Err(adapter_rejected(code)),
            Err(AdapterFailure::Unknown) => {
                let now = now_ms()?;
                let transaction = self.transaction()?;
                transaction
                    .execute(
                        "UPDATE brokered_members SET membership_state='degraded',actor_residency='unavailable',
                         updated_at_ms=?2 WHERE run_id=?1",
                        params![run_id.to_string(), now],
                    )
                    .map_err(internal)?;
                append_event(
                    &transaction,
                    session_id,
                    "specialist_release_unknown",
                    &sha256_hex(run_id.as_bytes()),
                    now,
                )?;
                transaction.commit().map_err(internal)?;
            }
        }
        self.member(session_id, run_id)
    }

    pub fn mark_primary_failed(
        &mut self,
        session_id: Uuid,
    ) -> Result<OrchestratedSessionSnapshot, MachineError> {
        let now = now_ms()?;
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE orchestrated_sessions SET status='degraded',revision=revision+1,updated_at_ms=?2
                 WHERE session_id=?1 AND status='active'",
                params![session_id.to_string(), now],
            )
            .map_err(internal)?;
        if changed == 1 {
            append_event(
                &transaction,
                session_id,
                "primary_unavailable",
                &sha256_hex(b"degraded"),
                now,
            )?;
        }
        transaction.commit().map_err(internal)?;
        self.session(session_id)
    }

    pub fn finish_session<A: OrchestrationAdapter>(
        &mut self,
        session_id: Uuid,
        abort: bool,
        adapter: &mut A,
    ) -> Result<OrchestratedSessionSnapshot, MachineError> {
        let session = self.session(session_id)?;
        if matches!(session.status.as_str(), "completed" | "aborted") {
            return Ok(session);
        }
        let active_tasks = self.session_tasks(session_id)?;
        if !abort
            && active_tasks
                .iter()
                .any(|task| !matches!(task.state.as_str(), "delivered" | "failed" | "cancelled"))
        {
            return Err(session_conflict(
                session_id,
                "graceful completion requires every Specialist task to be terminal and delivered",
            ));
        }
        if abort {
            for task in active_tasks {
                if !matches!(
                    task.state.as_str(),
                    "delivered" | "failed" | "interrupted_unknown" | "cancelled"
                ) {
                    let _ = self.cancel_task(session_id, task.task_id, adapter)?;
                }
            }
        }
        let transition = if abort { "aborting" } else { "completing" };
        let now = now_ms()?;
        let transaction = self.transaction()?;
        let changed = transaction
            .execute(
                "UPDATE orchestrated_sessions SET status=?2,revision=revision+1,updated_at_ms=?3
                 WHERE session_id=?1 AND status NOT IN ('completing','aborting','completed','aborted')",
                params![session_id.to_string(), transition, now],
            )
            .map_err(internal)?;
        if changed == 1 {
            append_event(
                &transaction,
                session_id,
                "session_close_started",
                &sha256_hex(transition.as_bytes()),
                now,
            )?;
        }
        transaction.commit().map_err(internal)?;
        for member in self.members(session_id)? {
            if member.membership_state != "retired" {
                let released = self.release_specialist(session_id, member.run_id, adapter)?;
                if released.membership_state != "retired" {
                    return Err(session_conflict(
                        session_id,
                        "session close requires every Specialist release to be authoritative",
                    ));
                }
            }
        }
        let terminal = if abort { "aborted" } else { "completed" };
        let now = now_ms()?;
        let transaction = self.transaction()?;
        transaction
            .execute(
                "UPDATE orchestrated_sessions SET status=?2,revision=revision+1,updated_at_ms=?3
                 WHERE session_id=?1",
                params![session_id.to_string(), terminal, now],
            )
            .map_err(internal)?;
        append_event(
            &transaction,
            session_id,
            "session_closed",
            &sha256_hex(terminal.as_bytes()),
            now,
        )?;
        transaction.commit().map_err(internal)?;
        self.session(session_id)
    }

    pub fn reconcile_unknown_work(&mut self, session_id: Uuid) -> Result<(), MachineError> {
        let now = now_ms()?;
        let transaction = self.transaction()?;
        let spawn_changes = transaction
            .execute(
                "UPDATE brokered_spawn_operations SET state='recovery_required',
                 safe_error_code='SPECIALIST_PUBLICATION_UNKNOWN',updated_at_ms=?2
                 WHERE session_id=?1 AND state='provisioning'",
                params![session_id.to_string(), now],
            )
            .map_err(internal)?;
        let member_changes = transaction
            .execute(
                "UPDATE brokered_members SET membership_state='degraded',actor_residency='unavailable',
                 updated_at_ms=?2 WHERE session_id=?1 AND membership_state='provisioning'
                 AND spawn_operation_id IN (
                   SELECT operation_id FROM brokered_spawn_operations
                   WHERE session_id=?1 AND state='recovery_required'
                 )",
                params![session_id.to_string(), now],
            )
            .map_err(internal)?;
        let task_changes = transaction
            .execute(
                "UPDATE brokered_tasks SET state='interrupted_unknown',
                 safe_error_code='TARGET_TURN_OUTCOME_UNKNOWN',updated_at_ms=?2
                 WHERE session_id=?1 AND state IN ('dispatching','running')",
                params![session_id.to_string(), now],
            )
            .map_err(internal)?;
        if spawn_changes + member_changes + task_changes != 0 {
            append_event(
                &transaction,
                session_id,
                "orchestration_unknown_reconciled",
                &sha256_hex(format!("{spawn_changes}:{member_changes}:{task_changes}").as_bytes()),
                now,
            )?;
        }
        transaction.commit().map_err(internal)
    }

    fn authorize_primary(
        &self,
        context: &PrimaryCallContext,
    ) -> Result<OrchestratedSessionSnapshot, MachineError> {
        let session = self.session(context.session_id)?;
        if session.root_run_id != context.source_run_id || session.status != "active" {
            return Err(session_conflict(
                context.session_id,
                "tool caller is not the active session Primary",
            ));
        }
        Ok(session)
    }

    fn spawn(&self, operation_id: Uuid) -> Result<SpecialistOperationSnapshot, MachineError> {
        let row = self
            .connection
            .query_row(
                "SELECT role_ref,state,child_run_id,approval_request_id,safe_error_code
                 FROM brokered_spawn_operations WHERE operation_id=?1",
                [operation_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(|| specialist_not_member(operation_id))?;
        Ok(SpecialistOperationSnapshot {
            operation_id,
            role_ref: row.0,
            state: row.1,
            specialist_run_id: row.2.as_deref().map(parse_uuid).transpose()?,
            approval_request_id: row.3.as_deref().map(parse_uuid).transpose()?,
            reused: false,
            safe_error_code: row.4,
        })
    }

    fn load_spawn_by_key(
        &self,
        session_id: Uuid,
        key: &str,
    ) -> Result<Option<SpecialistOperationSnapshot>, MachineError> {
        let id = self
            .connection
            .query_row(
                "SELECT operation_id FROM brokered_spawn_operations
                 WHERE session_id=?1 AND idempotency_key=?2",
                params![session_id.to_string(), key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(internal)?;
        id.map(|id| self.spawn(parse_uuid(&id)?)).transpose()
    }

    fn spawn_request_digest(&self, operation_id: Uuid) -> Result<String, MachineError> {
        self.connection
            .query_row(
                "SELECT request_sha256 FROM brokered_spawn_operations WHERE operation_id=?1",
                [operation_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)
    }

    fn load_reuse_receipt(
        &self,
        session_id: Uuid,
        idempotency_key: &str,
        request_sha256: &str,
    ) -> Result<Option<SpecialistOperationSnapshot>, MachineError> {
        let receipt = self
            .connection
            .query_row(
                "SELECT request_sha256,spawn_operation_id
                 FROM brokered_specialist_reuse_receipts
                 WHERE session_id=?1 AND idempotency_key=?2",
                params![session_id.to_string(), idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((recorded_sha256, operation_id)) = receipt else {
            return Ok(None);
        };
        if recorded_sha256 != request_sha256 {
            return Err(idempotency_conflict(idempotency_key));
        }
        let mut operation = self.spawn(parse_uuid(&operation_id)?)?;
        operation.reused = true;
        Ok(Some(operation))
    }

    fn record_reuse_receipt(
        &mut self,
        session_id: Uuid,
        idempotency_key: &str,
        request_sha256: &str,
        spawn_operation_id: Uuid,
    ) -> Result<SpecialistOperationSnapshot, MachineError> {
        let now = now_ms()?;
        let transaction = self.transaction()?;
        transaction
            .execute(
                "INSERT INTO brokered_specialist_reuse_receipts(
                   session_id,idempotency_key,request_sha256,spawn_operation_id,created_at_ms
                 ) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(session_id,idempotency_key) DO NOTHING",
                params![
                    session_id.to_string(),
                    idempotency_key,
                    request_sha256,
                    spawn_operation_id.to_string(),
                    now
                ],
            )
            .map_err(internal)?;
        let (recorded_sha256, recorded_operation_id) = transaction
            .query_row(
                "SELECT request_sha256,spawn_operation_id
                 FROM brokered_specialist_reuse_receipts
                 WHERE session_id=?1 AND idempotency_key=?2",
                params![session_id.to_string(), idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(internal)?;
        if recorded_sha256 != request_sha256 {
            return Err(idempotency_conflict(idempotency_key));
        }
        transaction.commit().map_err(internal)?;
        let mut operation = self.spawn(parse_uuid(&recorded_operation_id)?)?;
        operation.reused = true;
        Ok(operation)
    }

    fn spawn_session(&self, operation_id: Uuid) -> Result<Uuid, MachineError> {
        let value: String = self
            .connection
            .query_row(
                "SELECT session_id FROM brokered_spawn_operations WHERE operation_id=?1",
                [operation_id.to_string()],
                |row| row.get(0),
            )
            .map_err(|_| specialist_not_member(operation_id))?;
        parse_uuid(&value)
    }

    fn member(
        &self,
        session_id: Uuid,
        run_id: Uuid,
    ) -> Result<BrokeredMemberSnapshot, MachineError> {
        self.members(session_id)?
            .into_iter()
            .find(|member| member.run_id == run_id)
            .ok_or_else(|| specialist_not_member(run_id))
    }

    fn task(&self, task_id: Uuid) -> Result<SpecialistTaskSnapshot, MachineError> {
        let row = self
            .connection
            .query_row(
                "SELECT target_run_id,state,target_turn_id,result_json,result_sha256,
                        result_artifact_id,safe_error_code,
                        (SELECT sequence FROM brokered_delivery_receipts
                         WHERE task_id=brokered_tasks.task_id)
                 FROM brokered_tasks WHERE task_id=?1",
                [task_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<u64>>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(|| specialist_task_not_found(task_id))?;
        Ok(SpecialistTaskSnapshot {
            task_id,
            specialist_run_id: parse_uuid(&row.0)?,
            state: row.1,
            target_turn_id: row.2,
            result: row
                .3
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(internal)?,
            result_sha256: row.4,
            result_artifact_ref: row.5.as_deref().map(parse_uuid).transpose()?,
            safe_error_code: row.6,
            delivery_sequence: row.7,
        })
    }

    fn task_by_key(
        &self,
        session_id: Uuid,
        key: &str,
    ) -> Result<Option<SpecialistTaskSnapshot>, MachineError> {
        self.connection
            .query_row(
                "SELECT task_id FROM brokered_tasks WHERE session_id=?1 AND idempotency_key=?2",
                params![session_id.to_string(), key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(internal)?
            .map(|id| self.task(parse_uuid(&id)?))
            .transpose()
    }

    fn task_request_digest(&self, task_id: Uuid) -> Result<String, MachineError> {
        self.connection
            .query_row(
                "SELECT request_sha256 FROM brokered_tasks WHERE task_id=?1",
                [task_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)
    }

    fn task_session(&self, task_id: Uuid) -> Result<Uuid, MachineError> {
        let value: String = self
            .connection
            .query_row(
                "SELECT session_id FROM brokered_tasks WHERE task_id=?1",
                [task_id.to_string()],
                |row| row.get(0),
            )
            .map_err(|_| specialist_task_not_found(task_id))?;
        parse_uuid(&value)
    }

    fn session_tasks(&self, session_id: Uuid) -> Result<Vec<SpecialistTaskSnapshot>, MachineError> {
        let ids = {
            let mut statement = self
                .connection
                .prepare("SELECT task_id FROM brokered_tasks WHERE session_id=?1 ORDER BY created_at_ms,task_id")
                .map_err(internal)?;
            statement
                .query_map([session_id.to_string()], |row| row.get::<_, String>(0))
                .map_err(internal)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(internal)?
        };
        ids.into_iter()
            .map(|id| self.task(parse_uuid(&id)?))
            .collect()
    }

    fn resolve_target(
        &self,
        session_id: Uuid,
        target: &TargetSelector,
    ) -> Result<Uuid, MachineError> {
        match target {
            TargetSelector::Run { run_id } => {
                let _ = self.member(session_id, *run_id)?;
                Ok(*run_id)
            }
            TargetSelector::Role { role_ref } => {
                let mut matches = self
                    .members(session_id)?
                    .into_iter()
                    .filter(|member| {
                        member.role_ref == *role_ref && member.membership_state == "active"
                    })
                    .map(|member| {
                        let state = self.member_task_state(member.run_id)?;
                        let rank = match state.as_str() {
                            "idle" => 0_u8,
                            "queued" => 1,
                            "running" => 2,
                            _ => 3,
                        };
                        Ok((rank, member.run_id))
                    })
                    .collect::<Result<Vec<_>, MachineError>>()?;
                matches.sort_unstable();
                let selected = matches.first().map(|(_, run_id)| *run_id).ok_or_else(|| {
                    policy_denied(role_ref, "role target has no active Specialist")
                })?;
                Ok(selected)
            }
        }
    }

    fn member_task_state(&self, run_id: Uuid) -> Result<String, MachineError> {
        let state = self
            .connection
            .query_row(
                "SELECT state FROM brokered_tasks WHERE target_run_id=?1
                 ORDER BY created_at_ms DESC LIMIT 1",
                [run_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(internal)?;
        Ok(match state.as_deref() {
            None | Some("delivered" | "failed" | "cancelled" | "interrupted_unknown") => "idle",
            Some("accepted" | "queued" | "claimed") => "queued",
            Some("dispatching" | "running" | "completed_not_delivered") => "running",
            Some(_) => "unavailable",
        }
        .to_owned())
    }

    fn member_pending_count(&self, run_id: Uuid) -> Result<u64, MachineError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM brokered_tasks WHERE target_run_id=?1
                 AND state IN ('accepted','queued','claimed','dispatching','running','completed_not_delivered')",
                [run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)
    }

    fn tool_result(
        &self,
        context: &PrimaryCallContext,
        request_sha256: &str,
    ) -> Result<Option<Value>, MachineError> {
        let row = self
            .connection
            .query_row(
                "SELECT source_run_id,source_turn_id,idempotency_key,request_sha256,response_json
                 FROM brokered_tool_results
                 WHERE session_id=?1 AND source_tool_call_id=?2",
                params![context.session_id.to_string(), context.source_tool_call_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(internal)?;
        match row {
            None => Ok(None),
            Some((run, turn, key, observed, response))
                if run == context.source_run_id.to_string()
                    && turn == context.source_turn_id
                    && key == context.idempotency_key
                    && observed == request_sha256 =>
            {
                serde_json::from_str(&response).map(Some).map_err(internal)
            }
            Some(_) => Err(idempotency_conflict(&context.source_tool_call_id)),
        }
    }

    fn record_tool_result(
        &mut self,
        context: &PrimaryCallContext,
        request_sha256: &str,
        response: &Value,
    ) -> Result<(), MachineError> {
        self.connection
            .execute(
                "INSERT INTO brokered_tool_results(
                   session_id,source_run_id,source_turn_id,source_tool_call_id,idempotency_key,
                   request_sha256,response_json,created_at_ms
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    context.session_id.to_string(),
                    context.source_run_id.to_string(),
                    context.source_turn_id,
                    context.source_tool_call_id,
                    context.idempotency_key,
                    request_sha256,
                    canonical_string(response)?,
                    now_ms()?
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    fn transaction(&mut self) -> Result<Transaction<'_>, MachineError> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(internal)
    }
}

const SCHEMA: &str = "
PRAGMA foreign_keys=ON;
PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS orchestrated_sessions(
  session_id TEXT PRIMARY KEY,
  bootstrap_operation_id TEXT NOT NULL UNIQUE,
  root_run_id TEXT NOT NULL UNIQUE,
  workspace_id TEXT NOT NULL,
  status TEXT NOT NULL,
  approval_policy TEXT NOT NULL,
  policy_json TEXT NOT NULL,
  policy_sha256 TEXT NOT NULL,
  revision INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS aggregate_bootstrap_operations(
  operation_id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL UNIQUE,
  workspace_id TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_sha256 TEXT NOT NULL,
  state TEXT NOT NULL,
  safe_error_code TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(workspace_id,idempotency_key),
  FOREIGN KEY(session_id) REFERENCES orchestrated_sessions(session_id)
) STRICT;
CREATE TABLE IF NOT EXISTS brokered_members(
  run_id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  parent_run_id TEXT NOT NULL,
  role_ref TEXT NOT NULL,
  role_snapshot_sha256 TEXT NOT NULL,
  agent_configuration_json TEXT NOT NULL,
  agent_configuration_sha256 TEXT NOT NULL,
  controller_binding_json TEXT NOT NULL,
  spawn_operation_id TEXT NOT NULL UNIQUE,
  spawned_by_turn_id TEXT,
  membership_state TEXT NOT NULL,
  actor_residency TEXT NOT NULL,
  activation_policy TEXT NOT NULL,
  requested_access TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  FOREIGN KEY(session_id) REFERENCES orchestrated_sessions(session_id)
) STRICT;
CREATE TABLE IF NOT EXISTS brokered_spawn_operations(
  operation_id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_sha256 TEXT NOT NULL,
  request_json TEXT NOT NULL,
  role_ref TEXT NOT NULL,
  parent_run_id TEXT NOT NULL,
  child_run_id TEXT,
  approval_request_id TEXT,
  state TEXT NOT NULL,
  safe_error_code TEXT,
  source_turn_id TEXT NOT NULL,
  source_tool_call_id TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(session_id,idempotency_key),
  FOREIGN KEY(session_id) REFERENCES orchestrated_sessions(session_id)
) STRICT;
CREATE TABLE IF NOT EXISTS brokered_specialist_reuse_receipts(
  session_id TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_sha256 TEXT NOT NULL,
  spawn_operation_id TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY(session_id,idempotency_key),
  FOREIGN KEY(session_id) REFERENCES orchestrated_sessions(session_id),
  FOREIGN KEY(spawn_operation_id) REFERENCES brokered_spawn_operations(operation_id)
) STRICT;
CREATE TABLE IF NOT EXISTS brokered_tasks(
  task_id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_sha256 TEXT NOT NULL,
  source_run_id TEXT NOT NULL,
  source_turn_id TEXT NOT NULL,
  source_tool_call_id TEXT NOT NULL,
  target_run_id TEXT NOT NULL,
  request_json TEXT NOT NULL,
  target_turn_id TEXT,
  result_json TEXT,
  result_sha256 TEXT,
  result_artifact_id TEXT,
  state TEXT NOT NULL,
  safe_error_code TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(session_id,idempotency_key),
  FOREIGN KEY(session_id) REFERENCES orchestrated_sessions(session_id),
  FOREIGN KEY(target_run_id) REFERENCES brokered_members(run_id)
) STRICT;
CREATE TABLE IF NOT EXISTS brokered_delivery_receipts(
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL UNIQUE,
  delivered_at_ms INTEGER NOT NULL,
  FOREIGN KEY(task_id) REFERENCES brokered_tasks(task_id)
) STRICT;
CREATE TABLE IF NOT EXISTS orchestration_events(
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload_sha256 TEXT NOT NULL,
  previous_hash TEXT NOT NULL,
  event_hash TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  FOREIGN KEY(session_id) REFERENCES orchestrated_sessions(session_id)
) STRICT;
CREATE TABLE IF NOT EXISTS brokered_tool_results(
  session_id TEXT NOT NULL,
  source_run_id TEXT NOT NULL,
  source_turn_id TEXT NOT NULL,
  source_tool_call_id TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_sha256 TEXT NOT NULL,
  response_json TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY(session_id,source_tool_call_id),
  FOREIGN KEY(session_id) REFERENCES orchestrated_sessions(session_id)
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS one_active_brokered_task_per_member
  ON brokered_tasks(target_run_id)
  WHERE state IN ('accepted','dispatching','running');
";

fn append_event(
    transaction: &Transaction<'_>,
    session_id: Uuid,
    kind: &str,
    payload_sha256: &str,
    now: i64,
) -> Result<(), MachineError> {
    let previous = transaction
        .query_row(
            "SELECT event_hash FROM orchestration_events WHERE session_id=?1
             ORDER BY sequence DESC LIMIT 1",
            [session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(internal)?
        .unwrap_or_else(|| "0".repeat(64));
    let event_hash =
        sha256_hex(format!("{previous}\0{session_id}\0{kind}\0{payload_sha256}\0{now}").as_bytes());
    transaction
        .execute(
            "INSERT INTO orchestration_events(
               session_id,kind,payload_sha256,previous_hash,event_hash,created_at_ms
             ) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                session_id.to_string(),
                kind,
                payload_sha256,
                previous,
                event_hash,
                now
            ],
        )
        .map_err(internal)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_member(
    transaction: &Transaction<'_>,
    context: &PrimaryCallContext,
    request: &RequestSpecialist,
    role: &InstalledSpecialistRole,
    operation_id: Uuid,
    run_id: Uuid,
    controller: &ControllerBinding,
    now: i64,
) -> Result<(), MachineError> {
    transaction
        .execute(
            "INSERT INTO brokered_members(
               run_id,session_id,parent_run_id,role_ref,role_snapshot_sha256,
               agent_configuration_json,agent_configuration_sha256,controller_binding_json,
               spawn_operation_id,spawned_by_turn_id,membership_state,actor_residency,
               activation_policy,requested_access,created_at_ms,updated_at_ms
             ) VALUES(?1,?2,?2,?3,?4,?5,?6,?7,?8,?9,'provisioning','unstarted',?10,?11,?12,?12)",
            params![
                run_id.to_string(),
                context.session_id.to_string(),
                role.role_ref,
                digest_value(role)?,
                canonical_string(&role.agent_configuration)?,
                crate::run::agent_configuration_digest(&role.agent_configuration)
                    .map_err(internal)?,
                canonical_string(controller)?,
                operation_id.to_string(),
                context.source_turn_id,
                role.activation_policy,
                request.requested_access,
                now
            ],
        )
        .map_err(internal)?;
    Ok(())
}

fn task_state_from_transaction(
    transaction: &Transaction<'_>,
    task_id: Uuid,
) -> Result<String, MachineError> {
    transaction
        .query_row(
            "SELECT state FROM brokered_tasks WHERE task_id=?1",
            [task_id.to_string()],
            |row| row.get(0),
        )
        .map_err(internal)
}

fn validate_context(context: &PrimaryCallContext) -> Result<(), MachineError> {
    checked(&context.source_turn_id, 256, "source_turn_id")?;
    checked(&context.source_tool_call_id, 256, "source_tool_call_id")?;
    checked(&context.idempotency_key, 256, "idempotency_key")
}

fn validate_tool_request_shape(payload: &Value) -> Result<(), MachineError> {
    let object = payload.as_object().ok_or_else(|| {
        MachineError::invalid_argument(
            "tool_payload",
            "payload does not match the private orchestration tool contract",
        )
    })?;
    let operation = object.get("operation").and_then(Value::as_str);
    let allowed: &[&str] = match operation {
        Some("request_specialist") => &[
            "operation",
            "role_ref",
            "objective",
            "expected_output",
            "requested_access",
            "deadline_seconds",
        ],
        Some("await_specialist_operations") => &[
            "operation",
            "operation_ids",
            "return_when",
            "transport_wait_seconds",
        ],
        Some("list_specialists") => &["operation"],
        Some("assign_specialist_task") => &[
            "operation",
            "target",
            "objective",
            "context_refs",
            "expected_output",
            "execution_intent",
            "blocking",
            "deadline_seconds",
        ],
        Some("await_specialist_tasks") => &[
            "operation",
            "task_ids",
            "return_when",
            "transport_wait_seconds",
        ],
        Some("collect_specialist_results") => &["operation", "limit"],
        Some("cancel_specialist_task") => &["operation", "task_id", "reason"],
        Some("release_specialist") => &["operation", "run_id", "reason"],
        _ => &[],
    };
    if allowed.is_empty()
        || object.len() != allowed.len()
        || object.keys().any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(MachineError::invalid_argument(
            "tool_payload",
            "payload does not match the private orchestration tool contract",
        ));
    }
    Ok(())
}

fn validate_request_specialist(request: &RequestSpecialist) -> Result<(), MachineError> {
    checked(&request.role_ref, 64, "role_ref")?;
    checked(&request.objective, 65_536, "objective")?;
    checked_list(&request.expected_output, 32, 512, "expected_output")?;
    access(&request.requested_access)?;
    if !(1..=3_600).contains(&request.deadline_seconds) {
        return Err(MachineError::invalid_argument(
            "deadline_seconds",
            "deadline must be 1 to 3600 seconds",
        ));
    }
    Ok(())
}

fn validate_assign_task(request: &AssignSpecialistTask) -> Result<(), MachineError> {
    checked(&request.objective, 65_536, "objective")?;
    checked_list(&request.expected_output, 32, 512, "expected_output")?;
    if request.context_refs.len() > 64 {
        return Err(MachineError::invalid_argument(
            "context_refs",
            "at most 64 context references are allowed",
        ));
    }
    access(&request.requested_access)?;
    if !(1..=86_400).contains(&request.deadline_seconds) {
        return Err(MachineError::invalid_argument(
            "deadline_seconds",
            "deadline must be 1 to 86400 seconds",
        ));
    }
    Ok(())
}

fn validate_wait(return_when: &str, seconds: u64) -> Result<(), MachineError> {
    if !matches!(return_when, "any" | "all") || !(1..=60).contains(&seconds) {
        return Err(MachineError::invalid_argument(
            "wait",
            "return_when and transport wait are outside the checked contract",
        ));
    }
    Ok(())
}

fn task_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed_not_delivered"
            | "delivered"
            | "failed"
            | "cancelled"
            | "interrupted_unknown"
            | "expired"
    )
}

fn task_summary(task: &SpecialistTaskSnapshot) -> Value {
    serde_json::json!({
        "task_id":task.task_id,
        "state":match task.state.as_str() {
            "completed_not_delivered" | "delivered" => "completed",
            other => other,
        },
        "target_run_id":task.specialist_run_id,
        "result_artifact_ref":task.result_artifact_ref,
        "safe_error_code":task.safe_error_code,
    })
}

fn checked_list(
    values: &[String],
    maximum: usize,
    bytes: usize,
    field: &str,
) -> Result<(), MachineError> {
    if values.is_empty() || values.len() > maximum {
        return Err(MachineError::invalid_argument(
            field,
            "list size is outside the checked bound",
        ));
    }
    for value in values {
        checked(value, bytes, field)?;
    }
    Ok(())
}

fn checked(value: &str, maximum: usize, field: &str) -> Result<(), MachineError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(MachineError::invalid_argument(
            field,
            "value is outside the checked bound",
        ));
    }
    Ok(())
}

fn digest(value: &str, field: &str) -> Result<(), MachineError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(MachineError::invalid_argument(
            field,
            "value must be a lowercase SHA-256",
        ));
    }
    Ok(())
}

fn access(value: &str) -> Result<(), MachineError> {
    if !matches!(
        value,
        "read_only" | "isolated_write" | "canonical_workspace_write"
    ) {
        return Err(MachineError::invalid_argument(
            "requested_access",
            "access is unsupported",
        ));
    }
    Ok(())
}

fn canonical_string(value: &impl Serialize) -> Result<String, MachineError> {
    let text = serde_json::to_string(value).map_err(internal)?;
    let bytes = canonicalize(&parse(&text).map_err(internal)?).map_err(internal)?;
    String::from_utf8(bytes).map_err(internal)
}

fn digest_value(value: &impl Serialize) -> Result<String, MachineError> {
    Ok(sha256_hex(canonical_string(value)?.as_bytes()))
}

fn now_ms() -> Result<i64, MachineError> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(internal)?
        .as_millis();
    i64::try_from(value).map_err(internal)
}

fn parse_uuid(value: &str) -> Result<Uuid, MachineError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| id.get_version_num() == 7)
        .ok_or_else(|| integrity("persisted identity is not UUIDv7"))
}

fn adapter_result(result: Result<(), AdapterFailure>) -> Result<(), MachineError> {
    match result {
        Ok(()) => Ok(()),
        Err(AdapterFailure::Rejected(code)) => Err(adapter_rejected(code)),
        Err(AdapterFailure::Unknown) => Err(MachineError::new(
            "SPECIALIST_WRITER_CONFLICT",
            "writer sequencing outcome is unknown",
            false,
            serde_json::json!({"required_action":"reconcile writer authority before retry"}),
        )),
    }
}

fn validate_member_credential(
    member: &BrokeredMemberSnapshot,
    credential: &BrokerCredential,
) -> Result<(), MachineError> {
    if credential.binding != member.controller_binding {
        return Err(integrity(
            "broker credential carrier differs from the immutable member binding",
        ));
    }
    Ok(())
}

fn approval_result(result: Result<(), AdapterFailure>) -> Result<(), MachineError> {
    match result {
        Ok(()) => Ok(()),
        Err(AdapterFailure::Rejected(code)) => Err(adapter_rejected(code)),
        Err(AdapterFailure::Unknown) => Err(MachineError::new(
            "INTERRUPTED_UNKNOWN",
            "approval interaction publication outcome is unknown",
            false,
            serde_json::json!({
                "required_action":"retry the same request to reconcile the approval interaction"
            }),
        )),
    }
}

fn adapter_rejected(code: String) -> MachineError {
    MachineError::new(
        code,
        "the bounded orchestration adapter rejected the operation",
        false,
        serde_json::json!({}),
    )
}

fn policy_denied(role: &str, reason: &str) -> MachineError {
    MachineError::new(
        "SPECIALIST_POLICY_DENIED",
        "the Specialist Policy denied the operation",
        false,
        serde_json::json!({"role_ref":role,"reason":reason}),
    )
}

fn idempotency_conflict(key: &str) -> MachineError {
    MachineError::new(
        "IDEMPOTENCY_CONFLICT",
        "idempotency key was already used with different input",
        false,
        serde_json::json!({"idempotency_key":key}),
    )
}

fn session_not_found(session_id: Uuid) -> MachineError {
    MachineError::new(
        "RUN_NOT_FOUND",
        "Orchestrated Session was not found",
        false,
        serde_json::json!({"session_id":session_id}),
    )
}

fn session_conflict(session_id: Uuid, reason: &str) -> MachineError {
    MachineError::new(
        "RUN_STATE_CONFLICT",
        "Orchestrated Session state conflicts with the operation",
        false,
        serde_json::json!({"session_id":session_id,"reason":reason}),
    )
}

fn specialist_not_member(run_id: Uuid) -> MachineError {
    MachineError::new(
        "SPECIALIST_NOT_MEMBER",
        "Specialist is not an active aggregate member",
        false,
        serde_json::json!({"run_id":run_id}),
    )
}

fn specialist_task_not_found(task_id: Uuid) -> MachineError {
    MachineError::new(
        "SPECIALIST_TASK_NOT_FOUND",
        "Specialist task was not found",
        false,
        serde_json::json!({"task_id":task_id}),
    )
}

fn interrupted_unknown(task_id: Uuid) -> MachineError {
    MachineError::new(
        "INTERRUPTED_UNKNOWN",
        "Specialist task outcome is not authoritative and will not be replayed",
        false,
        serde_json::json!({"task_id":task_id}),
    )
}

fn task_failed(task_id: Uuid, code: &str) -> MachineError {
    MachineError::new(
        code,
        "Specialist task failed before a deliverable result was committed",
        false,
        serde_json::json!({"task_id":task_id}),
    )
}

fn integrity(reason: &str) -> MachineError {
    MachineError::new(
        "ORCHESTRATION_SCHEMA_UNSUPPORTED",
        "durable orchestration state failed validation",
        false,
        serde_json::json!({"reason":reason}),
    )
}

fn internal(error: impl ToString) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable orchestration operation failed",
        false,
        serde_json::json!({"reason":error.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Assurance, ExecutionLane, Purpose, PurposeKind};
    use crate::run::{AgentConfigurationSnapshot, InstructionSnapshot};
    use crate::specialist_policy::{
        InstalledSpecialistRole, RoleSource, RoleSourceReference, RoleSourceScope,
    };
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    struct FakeAdapter {
        calls: Vec<String>,
        approvals: BTreeSet<Uuid>,
        approval: Result<(), AdapterFailure>,
        publish: Result<(), AdapterFailure>,
        thread: Result<(), AdapterFailure>,
        dispatch: Result<CompletedTask, AdapterFailure>,
        cancel: Result<(), AdapterFailure>,
        release: Result<(), AdapterFailure>,
        writer: Result<(), AdapterFailure>,
        writer_acquire: Result<(), AdapterFailure>,
        observed_secret: Option<Vec<u8>>,
    }

    impl Default for FakeAdapter {
        fn default() -> Self {
            Self {
                calls: Vec::new(),
                approvals: BTreeSet::new(),
                approval: Ok(()),
                publish: Ok(()),
                thread: Ok(()),
                dispatch: Ok(CompletedTask {
                    turn_id: "turn-specialist".to_owned(),
                    result: serde_json::json!({"answer":"verified"}),
                }),
                cancel: Ok(()),
                release: Ok(()),
                writer: Ok(()),
                writer_acquire: Ok(()),
                observed_secret: None,
            }
        }
    }

    impl OrchestrationAdapter for FakeAdapter {
        fn publish_approval_request(
            &mut self,
            session_id: Uuid,
            operation_id: Uuid,
            approval_request_id: Uuid,
            request: &RequestSpecialist,
        ) -> Result<(), AdapterFailure> {
            if self.approvals.insert(approval_request_id) {
                self.calls.push(format!(
                    "approval:{session_id}:{operation_id}:{approval_request_id}:{}",
                    request.role_ref
                ));
            }
            self.approval.clone()
        }

        fn publish_specialist(
            &mut self,
            plan: &BrokeredRunPlan,
            credential: &BrokerCredential,
        ) -> Result<(), AdapterFailure> {
            self.calls.push(format!("publish:{}", plan.run_id));
            self.observed_secret = Some(credential.capability().to_vec());
            self.publish.clone()
        }

        fn create_thread(
            &mut self,
            plan: &BrokeredRunPlan,
            _credential: &BrokerCredential,
        ) -> Result<(), AdapterFailure> {
            self.calls.push(format!("thread:{}", plan.run_id));
            self.thread.clone()
        }

        fn dispatch_task(
            &mut self,
            member: &BrokeredMemberSnapshot,
            _request: &AssignSpecialistTask,
            credential: &BrokerCredential,
        ) -> Result<CompletedTask, AdapterFailure> {
            self.calls.push(format!("dispatch:{}", member.run_id));
            assert_eq!(
                credential.binding.capability_sha256,
                controller_capability_digest(credential.capability())
            );
            self.dispatch.clone()
        }

        fn cancel_task(
            &mut self,
            task: &SpecialistTaskSnapshot,
            credential: &BrokerCredential,
        ) -> Result<(), AdapterFailure> {
            self.calls.push(format!("cancel:{}", task.task_id));
            assert_eq!(
                credential.binding.capability_sha256,
                controller_capability_digest(credential.capability())
            );
            self.cancel.clone()
        }

        fn release_specialist(
            &mut self,
            member: &BrokeredMemberSnapshot,
            credential: &BrokerCredential,
        ) -> Result<(), AdapterFailure> {
            self.calls.push(format!("release:{}", member.run_id));
            assert_eq!(
                credential.binding.capability_sha256,
                controller_capability_digest(credential.capability())
            );
            self.release.clone()
        }

        fn release_writer(&mut self, run_id: Uuid) -> Result<(), AdapterFailure> {
            self.calls.push(format!("writer-release:{run_id}"));
            self.writer.clone()
        }

        fn verify_writer_none(&mut self) -> Result<(), AdapterFailure> {
            self.calls.push("writer-none".to_owned());
            self.writer.clone()
        }

        fn acquire_writer(&mut self, run_id: Uuid) -> Result<(), AdapterFailure> {
            self.calls.push(format!("writer-acquire:{run_id}"));
            self.writer_acquire.clone()
        }
    }

    struct OneFault {
        barrier: OrchestrationBarrier,
        fired: Mutex<bool>,
    }

    impl OrchestrationFaultInjector for OneFault {
        fn check(&self, barrier: OrchestrationBarrier) -> Result<(), MachineError> {
            let mut fired = self.fired.lock().unwrap();
            if barrier == self.barrier && !*fired {
                *fired = true;
                return Err(MachineError::new(
                    "INTERNAL_ERROR",
                    "injected orchestration fault",
                    false,
                    serde_json::json!({"barrier":format!("{barrier:?}")}),
                ));
            }
            Ok(())
        }
    }

    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dolgorae-orchestration-{}", Uuid::now_v7()))
    }

    fn policy(approval: &str) -> InstalledSpecialistPolicy {
        let role_source = RoleSource {
            schema_version: 1,
            name: "reviewer".to_owned(),
            display_name: "Reviewer".to_owned(),
            description: "Reviews the requested change.".to_owned(),
            instructions: "Inspect the evidence and return a bounded result.".to_owned(),
        };
        let role_source_sha256 = digest_value(&role_source).unwrap();
        InstalledSpecialistPolicy {
            schema_version: 2,
            policy_name: format!("{approval}-policy"),
            revision: 1,
            approval_policy: approval.to_owned(),
            max_active_specialists: 4,
            roles: vec![InstalledSpecialistRole {
                role_ref: "reviewer".to_owned(),
                role_source: RoleSourceReference {
                    scope: RoleSourceScope::Project,
                    name: "reviewer".to_owned(),
                },
                role_source_sha256,
                role: role_source,
                agent_configuration: AgentConfigurationSnapshot {
                    schema_version: 2,
                    runtime_profile: "test".to_owned(),
                    runtime_profile_snapshot_sha256: "a".repeat(64),
                    model: "test-model".to_owned(),
                    default_effort: "medium".to_owned(),
                    purpose: Purpose {
                        kind: PurposeKind::Review,
                        external_label: None,
                    },
                    required_capabilities: Vec::new(),
                    role_reference: Some("reviewer".to_owned()),
                    normalized_instructions: "Inspect the evidence and return a bounded result."
                        .to_owned(),
                    instructions: InstructionSnapshot {
                        schema: "dolgorae.instructions/v1".to_owned(),
                        common_prefix_version: 1,
                        mode_prefix_version: 1,
                        purpose_prefix_version: 1,
                        normalized_byte_length: 49,
                        normalized_sha256: sha256_hex(
                            b"Inspect the evidence and return a bounded result.",
                        ),
                    },
                    execution_lane: ExecutionLane::Dedicated,
                    required_assurance: Assurance::BestEffortPersonalAlpha,
                    native_subagent_policy: "enabled".to_owned(),
                },
                max_active_instances: 2,
                reuse_policy: "never".to_owned(),
                allowed_access: vec![
                    "read_only".to_owned(),
                    "canonical_workspace_write".to_owned(),
                ],
                activation_policy: "keep_resident".to_owned(),
                primary_may_request: true,
                collaboration_source: false,
                collaboration_target: false,
                auto_approve_when_fully_delegated: true,
            }],
        }
    }

    fn active_session(
        root: &Path,
        approval: &str,
    ) -> (OrchestrationStore, OrchestratedSessionSnapshot) {
        active_session_with_policy(root, policy(approval))
    }

    fn active_session_with_policy(
        root: &Path,
        policy: InstalledSpecialistPolicy,
    ) -> (OrchestrationStore, OrchestratedSessionSnapshot) {
        let mut store = OrchestrationStore::open(root).unwrap();
        let run_id = new_uuid_v7();
        store
            .prepare_session("workspace", run_id, "bootstrap", &"b".repeat(64), &policy)
            .unwrap();
        store.mark_primary_intent(run_id).unwrap();
        let session = store.finish_primary_publication(run_id, Ok(())).unwrap();
        (store, session)
    }

    fn context(session: Uuid, key: &str) -> PrimaryCallContext {
        PrimaryCallContext {
            session_id: session,
            source_run_id: session,
            source_turn_id: "turn-primary".to_owned(),
            source_tool_call_id: format!("tool-{key}"),
            idempotency_key: key.to_owned(),
        }
    }

    fn request(access: &str) -> RequestSpecialist {
        RequestSpecialist {
            role_ref: "reviewer".to_owned(),
            objective: "Review the implementation.".to_owned(),
            expected_output: vec!["A bounded verdict".to_owned()],
            requested_access: access.to_owned(),
            deadline_seconds: 60,
        }
    }

    fn task_request(child: Uuid, access: &str) -> AssignSpecialistTask {
        AssignSpecialistTask {
            specialist_run_id: child,
            objective: "Apply the verified change.".to_owned(),
            context_refs: Vec::new(),
            expected_output: vec!["Result".to_owned()],
            requested_access: access.to_owned(),
            deadline_seconds: 60,
        }
    }

    fn active_member(root: &Path) -> (OrchestrationStore, OrchestratedSessionSnapshot, Uuid) {
        let (mut store, session) = active_session(root, "fully_delegated");
        let mut adapter = FakeAdapter::default();
        let operation = store
            .request_specialist(
                &context(session.session_id, "member"),
                &request("read_only"),
                &mut adapter,
            )
            .unwrap();
        (store, session, operation.specialist_run_id.unwrap())
    }

    #[test]
    fn bootstrap_is_exactly_replayable_and_snapshot_is_immutable() {
        let root = root();
        let mut store = OrchestrationStore::open(&root).unwrap();
        let run_id = new_uuid_v7();
        let original = policy("fully_delegated");
        let prepared = store
            .prepare_session("workspace", run_id, "same", &"c".repeat(64), &original)
            .unwrap();
        assert_eq!(prepared.status, "creating");
        let replay = store
            .prepare_session("workspace", run_id, "same", &"c".repeat(64), &original)
            .unwrap();
        assert_eq!(
            replay.bootstrap_operation_id,
            prepared.bootstrap_operation_id
        );
        assert_eq!(
            store
                .prepare_session("workspace", run_id, "same", &"d".repeat(64), &original)
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        store.mark_primary_intent(run_id).unwrap();
        let ready = store.finish_primary_publication(run_id, Ok(())).unwrap();
        assert_eq!(ready.status, "active");
        assert_eq!(ready.specialist_policy, original);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fully_delegated_flow_is_idempotent_retains_results_and_sequences_writer_transfer() {
        let root = root();
        let (mut store, session) = active_session(&root, "fully_delegated");
        let mut adapter = FakeAdapter::default();
        let call = context(session.session_id, "spawn");
        let operation = store
            .request_specialist(&call, &request("canonical_workspace_write"), &mut adapter)
            .unwrap();
        assert_eq!(operation.state, "ready");
        let child = operation.specialist_run_id.unwrap();
        let carrier = root
            .join("orchestration/broker-credentials")
            .join(format!("{child}.json"));
        assert_eq!(std::fs::metadata(&carrier).unwrap().mode() & 0o777, 0o600);
        let adapter_calls = adapter.calls.len();
        assert_eq!(
            store
                .request_specialist(&call, &request("canonical_workspace_write"), &mut adapter)
                .unwrap()
                .operation_id,
            operation.operation_id
        );
        assert_eq!(adapter.calls.len(), adapter_calls);
        let mut drift = request("canonical_workspace_write");
        drift.objective.push_str(" Changed.");
        assert_eq!(
            store
                .request_specialist(&call, &drift, &mut adapter)
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );

        let task_call = context(session.session_id, "task");
        let task_request = AssignSpecialistTask {
            specialist_run_id: child,
            objective: "Apply the verified change.".to_owned(),
            context_refs: Vec::new(),
            expected_output: vec!["Result".to_owned()],
            requested_access: "canonical_workspace_write".to_owned(),
            deadline_seconds: 60,
        };
        let task = store
            .assign_task(&task_call, &task_request, &mut adapter)
            .unwrap();
        assert_eq!(task.state, "completed_not_delivered");
        let writer_calls = adapter
            .calls
            .iter()
            .filter(|call| call.starts_with("writer-") || call.as_str() == "writer-none")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            writer_calls,
            vec![
                format!("writer-release:{}", session.root_run_id),
                "writer-none".to_owned(),
                format!("writer-acquire:{child}"),
            ]
        );
        let later_task = store
            .assign_task(
                &context(session.session_id, "later-task"),
                &AssignSpecialistTask {
                    specialist_run_id: child,
                    objective: "Return a second result.".to_owned(),
                    context_refs: Vec::new(),
                    expected_output: vec!["Second result".to_owned()],
                    requested_access: "read_only".to_owned(),
                    deadline_seconds: 60,
                },
                &mut adapter,
            )
            .unwrap();
        let first = store.collect_results(session.session_id, 0, 1).unwrap();
        let second = store.collect_results(session.session_id, 0, 1).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].task_id, task.task_id);
        assert_eq!(first[0].state, "delivered");
        assert_eq!(
            store.task(later_task.task_id).unwrap().state,
            "completed_not_delivered"
        );
        let last = store
            .collect_results(session.session_id, first[0].delivery_sequence.unwrap(), 1)
            .unwrap();
        assert_eq!(last[0].task_id, later_task.task_id);
        assert_eq!(
            adapter
                .calls
                .iter()
                .filter(|call| call.starts_with("dispatch:"))
                .count(),
            2
        );

        let secret = adapter.observed_secret.clone().unwrap();
        let database = std::fs::read(EngagementStore::workspace_database_path(&root)).unwrap();
        assert!(
            !database
                .windows(secret.len())
                .any(|window| window == secret)
        );
        let terminal = store
            .finish_session(session.session_id, false, &mut adapter)
            .unwrap();
        assert_eq!(terminal.status, "completed");
        assert_eq!(terminal.composition_state, "standalone_primary");
        assert!(!carrier.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn approval_required_defers_identity_and_unknown_turns_never_replay() {
        let root = root();
        let (mut store, session) = active_session(&root, "user_approval_required");
        let mut adapter = FakeAdapter::default();
        let denied = store
            .request_specialist(
                &context(session.session_id, "denied-approval"),
                &request("read_only"),
                &mut adapter,
            )
            .unwrap();
        assert_eq!(
            store
                .decide_specialist(
                    &context(session.session_id, "deny-decision"),
                    denied.operation_id,
                    false,
                    &mut adapter,
                )
                .unwrap()
                .state,
            "denied"
        );
        assert_eq!(
            store
                .decide_specialist(
                    &context(session.session_id, "deny-replay"),
                    denied.operation_id,
                    false,
                    &mut adapter,
                )
                .unwrap()
                .state,
            "denied"
        );
        assert_eq!(
            store
                .decide_specialist(
                    &context(session.session_id, "deny-drift"),
                    denied.operation_id,
                    true,
                    &mut adapter,
                )
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        let unknown_call = context(session.session_id, "approval-unknown");
        adapter.approval = Err(AdapterFailure::Unknown);
        assert_eq!(
            store
                .request_specialist(&unknown_call, &request("read_only"), &mut adapter)
                .unwrap_err()
                .code,
            "INTERRUPTED_UNKNOWN"
        );
        adapter.approval = Ok(());
        assert_eq!(
            store
                .request_specialist(&unknown_call, &request("read_only"), &mut adapter)
                .unwrap()
                .state,
            "awaiting_approval"
        );
        let operation = store
            .request_specialist(
                &context(session.session_id, "approval"),
                &request("read_only"),
                &mut adapter,
            )
            .unwrap();
        assert_eq!(operation.state, "awaiting_approval");
        assert!(operation.specialist_run_id.is_none());
        let approval_calls = adapter.calls.len();
        assert_eq!(
            store
                .request_specialist(
                    &context(session.session_id, "approval"),
                    &request("read_only"),
                    &mut adapter,
                )
                .unwrap(),
            operation
        );
        assert_eq!(adapter.calls.len(), approval_calls);
        assert!(
            adapter
                .calls
                .iter()
                .all(|call| call.starts_with("approval:"))
        );
        let ready = store
            .decide_specialist(
                &context(session.session_id, "decision"),
                operation.operation_id,
                true,
                &mut adapter,
            )
            .unwrap();
        assert_eq!(ready.state, "ready");
        let child = ready.specialist_run_id.unwrap();
        let adapter_calls = adapter.calls.len();
        assert_eq!(
            store
                .decide_specialist(
                    &context(session.session_id, "approval-drift"),
                    operation.operation_id,
                    false,
                    &mut adapter,
                )
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        assert_eq!(
            store
                .decide_specialist(
                    &context(session.session_id, "approval-replay"),
                    operation.operation_id,
                    true,
                    &mut adapter,
                )
                .unwrap(),
            ready
        );
        assert_eq!(adapter.calls.len(), adapter_calls);
        adapter.dispatch = Err(AdapterFailure::Unknown);
        let task_call = context(session.session_id, "unknown-task");
        let task_request = AssignSpecialistTask {
            specialist_run_id: child,
            objective: "Run once.".to_owned(),
            context_refs: Vec::new(),
            expected_output: vec!["Result".to_owned()],
            requested_access: "read_only".to_owned(),
            deadline_seconds: 60,
        };
        let error = store
            .assign_task(&task_call, &task_request, &mut adapter)
            .unwrap_err();
        assert_eq!(error.code, "INTERRUPTED_UNKNOWN");
        let task = store
            .task_by_key(session.session_id, "unknown-task")
            .unwrap()
            .unwrap();
        assert_eq!(task.state, "interrupted_unknown");
        let calls = adapter.calls.len();
        assert_eq!(
            store
                .assign_task(&task_call, &task_request, &mut adapter)
                .unwrap_err()
                .code,
            "INTERRUPTED_UNKNOWN"
        );
        assert_eq!(adapter.calls.len(), calls);
        assert_eq!(
            store
                .mark_primary_failed(session.session_id)
                .unwrap()
                .status,
            "degraded"
        );
        assert_eq!(store.members(session.session_id).unwrap().len(), 1);
        adapter.release = Err(AdapterFailure::Unknown);
        assert_eq!(
            store
                .finish_session(session.session_id, true, &mut adapter)
                .unwrap_err()
                .code,
            "RUN_STATE_CONFLICT"
        );
        assert_eq!(
            store.session(session.session_id).unwrap().status,
            "aborting"
        );
        assert_eq!(
            store.members(session.session_id).unwrap()[0].membership_state,
            "degraded"
        );
        adapter.release = Ok(());
        assert_eq!(
            store
                .finish_session(session.session_id, true, &mut adapter)
                .unwrap()
                .status,
            "aborted"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_fault_boundaries_recover_without_duplicate_identity() {
        let root = root();
        let run_id = new_uuid_v7();
        let fault = Arc::new(OneFault {
            barrier: OrchestrationBarrier::AfterSessionCommit,
            fired: Mutex::new(false),
        });
        let mut store = OrchestrationStore::open_with_faults(&root, fault).unwrap();
        assert!(
            store
                .prepare_session(
                    "workspace",
                    run_id,
                    "bootstrap",
                    &"e".repeat(64),
                    &policy("fully_delegated")
                )
                .is_err()
        );
        drop(store);
        let mut store = OrchestrationStore::open(&root).unwrap();
        let replay = store
            .prepare_session(
                "workspace",
                run_id,
                "bootstrap",
                &"e".repeat(64),
                &policy("fully_delegated"),
            )
            .unwrap();
        assert_eq!(replay.session_id, run_id);
        store.mark_primary_intent(run_id).unwrap();
        store.finish_primary_publication(run_id, Ok(())).unwrap();

        let fault = Arc::new(OneFault {
            barrier: OrchestrationBarrier::AfterSpawnReservation,
            fired: Mutex::new(false),
        });
        drop(store);
        let mut store = OrchestrationStore::open_with_faults(&root, fault).unwrap();
        let mut adapter = FakeAdapter::default();
        assert!(
            store
                .request_specialist(
                    &context(run_id, "spawn-fault"),
                    &request("read_only"),
                    &mut adapter
                )
                .is_err()
        );
        assert!(adapter.calls.is_empty());
        drop(store);
        let mut store = OrchestrationStore::open(&root).unwrap();
        store.reconcile_unknown_work(run_id).unwrap();
        let reserved = store
            .load_spawn_by_key(run_id, "spawn-fault")
            .unwrap()
            .unwrap();
        assert_eq!(reserved.state, "requested");
        let child = reserved.specialist_run_id.unwrap();
        let operation = store
            .request_specialist(
                &context(run_id, "spawn-fault"),
                &request("read_only"),
                &mut adapter,
            )
            .unwrap();
        assert_eq!(operation.state, "ready");
        assert_eq!(operation.specialist_run_id, Some(child));
        assert_eq!(store.members(run_id).unwrap()[0].membership_state, "active");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn primary_publication_faults_replay_one_bootstrap_without_duplicate_events() {
        let first_root = root();
        let run_id = new_uuid_v7();
        let fault = Arc::new(OneFault {
            barrier: OrchestrationBarrier::BeforeSessionCommit,
            fired: Mutex::new(false),
        });
        let mut store = OrchestrationStore::open_with_faults(&first_root, fault).unwrap();
        assert!(
            store
                .prepare_session(
                    "workspace",
                    run_id,
                    "bootstrap",
                    &"f".repeat(64),
                    &policy("fully_delegated"),
                )
                .is_err()
        );
        drop(store);
        let mut store = OrchestrationStore::open(&first_root).unwrap();
        assert_eq!(
            store
                .prepare_session(
                    "workspace",
                    run_id,
                    "bootstrap",
                    &"f".repeat(64),
                    &policy("fully_delegated"),
                )
                .unwrap()
                .session_id,
            run_id
        );
        std::fs::remove_dir_all(first_root).unwrap();

        for barrier in [
            OrchestrationBarrier::BeforePrimaryIntent,
            OrchestrationBarrier::AfterPrimaryIntent,
        ] {
            let root = root();
            let run_id = new_uuid_v7();
            let mut store = OrchestrationStore::open(&root).unwrap();
            store
                .prepare_session(
                    "workspace",
                    run_id,
                    "bootstrap",
                    &"f".repeat(64),
                    &policy("fully_delegated"),
                )
                .unwrap();
            drop(store);
            let fault = Arc::new(OneFault {
                barrier,
                fired: Mutex::new(false),
            });
            let mut store = OrchestrationStore::open_with_faults(&root, fault).unwrap();
            assert!(store.mark_primary_intent(run_id).is_err());
            drop(store);
            let mut store = OrchestrationStore::open(&root).unwrap();
            store.mark_primary_intent(run_id).unwrap();
            assert_eq!(
                store
                    .finish_primary_publication(run_id, Ok(()))
                    .unwrap()
                    .status,
                "active"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
        for barrier in [
            OrchestrationBarrier::BeforePrimaryPublication,
            OrchestrationBarrier::AfterPrimaryPublication,
        ] {
            let root = root();
            let run_id = new_uuid_v7();
            let mut store = OrchestrationStore::open(&root).unwrap();
            store
                .prepare_session(
                    "workspace",
                    run_id,
                    "bootstrap",
                    &"f".repeat(64),
                    &policy("fully_delegated"),
                )
                .unwrap();
            store.mark_primary_intent(run_id).unwrap();
            drop(store);
            let fault = Arc::new(OneFault {
                barrier,
                fired: Mutex::new(false),
            });
            let mut store = OrchestrationStore::open_with_faults(&root, fault).unwrap();
            assert!(store.finish_primary_publication(run_id, Ok(())).is_err());
            drop(store);
            let mut store = OrchestrationStore::open(&root).unwrap();
            let event_count_before: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM orchestration_events WHERE session_id=?1",
                    [run_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                store
                    .finish_primary_publication(run_id, Ok(()))
                    .unwrap()
                    .status,
                "active"
            );
            let event_count_after: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM orchestration_events WHERE session_id=?1",
                    [run_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            if barrier == OrchestrationBarrier::AfterPrimaryPublication {
                assert_eq!(event_count_after, event_count_before);
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn ambiguous_specialist_publication_never_replays_external_effects() {
        let first_root = root();
        let (store, session) = active_session(&first_root, "fully_delegated");
        drop(store);
        let fault = Arc::new(OneFault {
            barrier: OrchestrationBarrier::BeforeSpawnReservation,
            fired: Mutex::new(false),
        });
        let mut store = OrchestrationStore::open_with_faults(&first_root, fault).unwrap();
        let mut adapter = FakeAdapter::default();
        let call = context(session.session_id, "before-spawn");
        assert!(
            store
                .request_specialist(&call, &request("read_only"), &mut adapter)
                .is_err()
        );
        assert!(
            store
                .load_spawn_by_key(session.session_id, "before-spawn")
                .unwrap()
                .is_none()
        );
        drop(store);
        let mut store = OrchestrationStore::open(&first_root).unwrap();
        assert_eq!(
            store
                .request_specialist(&call, &request("read_only"), &mut adapter)
                .unwrap()
                .state,
            "ready"
        );
        std::fs::remove_dir_all(first_root).unwrap();

        for (barrier, expected_calls) in [
            (OrchestrationBarrier::BeforeWorkerPublication, 0),
            (OrchestrationBarrier::AfterWorkerPublication, 1),
            (OrchestrationBarrier::BeforeThreadCreation, 1),
            (OrchestrationBarrier::AfterThreadCreation, 2),
        ] {
            let root = root();
            let (store, session) = active_session(&root, "fully_delegated");
            drop(store);
            let fault = Arc::new(OneFault {
                barrier,
                fired: Mutex::new(false),
            });
            let mut store = OrchestrationStore::open_with_faults(&root, fault).unwrap();
            let mut adapter = FakeAdapter::default();
            let call = context(session.session_id, "ambiguous-spawn");
            assert!(
                store
                    .request_specialist(&call, &request("read_only"), &mut adapter)
                    .is_err()
            );
            assert_eq!(adapter.calls.len(), expected_calls);
            drop(store);
            let mut store = OrchestrationStore::open(&root).unwrap();
            store.reconcile_unknown_work(session.session_id).unwrap();
            let operation = store
                .load_spawn_by_key(session.session_id, "ambiguous-spawn")
                .unwrap()
                .unwrap();
            assert_eq!(operation.state, "recovery_required");
            let calls = adapter.calls.len();
            assert_eq!(
                store
                    .request_specialist(&call, &request("read_only"), &mut adapter)
                    .unwrap()
                    .state,
                "recovery_required"
            );
            assert_eq!(adapter.calls.len(), calls);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn task_and_delivery_faults_resume_only_before_external_acceptance() {
        for (barrier, resumes) in [
            (OrchestrationBarrier::BeforeTaskDispatch, true),
            (OrchestrationBarrier::AfterTaskDispatch, false),
            (OrchestrationBarrier::BeforeResultAppend, false),
            (OrchestrationBarrier::AfterResultAppend, true),
        ] {
            let root = root();
            let (store, session, child) = active_member(&root);
            drop(store);
            let fault = Arc::new(OneFault {
                barrier,
                fired: Mutex::new(false),
            });
            let mut store = OrchestrationStore::open_with_faults(&root, fault).unwrap();
            let mut adapter = FakeAdapter::default();
            let call = context(session.session_id, "faulted-task");
            let task = task_request(child, "read_only");
            assert!(store.assign_task(&call, &task, &mut adapter).is_err());
            let dispatches = adapter
                .calls
                .iter()
                .filter(|entry| entry.starts_with("dispatch:"))
                .count();
            drop(store);
            let mut store = OrchestrationStore::open(&root).unwrap();
            if resumes {
                let completed = store.assign_task(&call, &task, &mut adapter).unwrap();
                assert_eq!(completed.state, "completed_not_delivered");
                assert_eq!(
                    adapter
                        .calls
                        .iter()
                        .filter(|entry| entry.starts_with("dispatch:"))
                        .count(),
                    1
                );
            } else {
                assert_eq!(
                    store
                        .assign_task(&call, &task, &mut adapter)
                        .unwrap_err()
                        .code,
                    "INTERRUPTED_UNKNOWN"
                );
                assert_eq!(
                    adapter
                        .calls
                        .iter()
                        .filter(|entry| entry.starts_with("dispatch:"))
                        .count(),
                    dispatches
                );
                store.reconcile_unknown_work(session.session_id).unwrap();
            }
            std::fs::remove_dir_all(root).unwrap();
        }

        for barrier in [
            OrchestrationBarrier::BeforeDeliveryReceipt,
            OrchestrationBarrier::AfterDeliveryReceipt,
        ] {
            let root = root();
            let (mut store, session, child) = active_member(&root);
            let mut adapter = FakeAdapter::default();
            store
                .assign_task(
                    &context(session.session_id, "delivery-task"),
                    &task_request(child, "read_only"),
                    &mut adapter,
                )
                .unwrap();
            drop(store);
            let fault = Arc::new(OneFault {
                barrier,
                fired: Mutex::new(false),
            });
            let mut store = OrchestrationStore::open_with_faults(&root, fault).unwrap();
            assert!(store.collect_results(session.session_id, 0, 8).is_err());
            drop(store);
            let mut store = OrchestrationStore::open(&root).unwrap();
            let delivered = store.collect_results(session.session_id, 0, 8).unwrap();
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].state, "delivered");
            assert_eq!(delivered[0].delivery_sequence, Some(1));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn writer_race_and_durable_integrity_fail_closed() {
        let root = root();
        let (mut store, session) = active_session(&root, "fully_delegated");
        let mut adapter = FakeAdapter::default();
        let operation = store
            .request_specialist(
                &context(session.session_id, "writer-member"),
                &request("canonical_workspace_write"),
                &mut adapter,
            )
            .unwrap();
        let child = operation.specialist_run_id.unwrap();
        adapter.writer_acquire = Err(AdapterFailure::Rejected("WRITER_BUSY".to_owned()));
        let call = context(session.session_id, "writer-task");
        let task = task_request(child, "canonical_workspace_write");
        assert_eq!(
            store
                .assign_task(&call, &task, &mut adapter)
                .unwrap_err()
                .code,
            "SPECIALIST_WRITER_CONFLICT"
        );
        let dispatches = adapter
            .calls
            .iter()
            .filter(|entry| entry.starts_with("dispatch:"))
            .count();
        assert_eq!(
            store
                .assign_task(&call, &task, &mut adapter)
                .unwrap_err()
                .code,
            "SPECIALIST_WRITER_CONFLICT"
        );
        assert_eq!(
            adapter
                .calls
                .iter()
                .filter(|entry| entry.starts_with("dispatch:"))
                .count(),
            dispatches
        );

        store
            .connection
            .execute(
                "UPDATE orchestration_events SET event_hash=?2 WHERE session_id=?1
                 AND sequence=(SELECT MAX(sequence) FROM orchestration_events WHERE session_id=?1)",
                params![session.session_id.to_string(), "0".repeat(64)],
            )
            .unwrap();
        assert_eq!(
            store.session(session.session_id).unwrap_err().code,
            "ORCHESTRATION_SCHEMA_UNSUPPORTED"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn self_consistent_replacement_carrier_cannot_change_member_authority() {
        let root = root();
        let (mut store, session) = active_session(&root, "fully_delegated");
        let mut adapter = FakeAdapter::default();
        let child = store
            .request_specialist(
                &context(session.session_id, "credential-member"),
                &request("read_only"),
                &mut adapter,
            )
            .unwrap()
            .specialist_run_id
            .unwrap();
        adapter.calls.clear();
        BrokerCredential::load(&store.credential_root, store.uid, child)
            .unwrap()
            .remove()
            .unwrap();
        let replacement =
            BrokerCredential::create(&store.credential_root, store.uid, child).unwrap();
        assert_ne!(
            replacement.binding,
            store
                .member(session.session_id, child)
                .unwrap()
                .controller_binding
        );
        assert_eq!(
            store
                .assign_task(
                    &context(session.session_id, "credential-task"),
                    &task_request(child, "read_only"),
                    &mut adapter,
                )
                .unwrap_err()
                .code,
            "ORCHESTRATION_SCHEMA_UNSUPPORTED"
        );
        assert!(adapter.calls.is_empty());
        drop(replacement);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn forged_primary_and_role_denial_fail_before_adapter_effects() {
        let root = root();
        let (mut store, session) = active_session(&root, "fully_delegated");
        let mut adapter = FakeAdapter::default();
        let mut forged = context(session.session_id, "forged");
        forged.source_run_id = new_uuid_v7();
        assert_eq!(
            store
                .request_specialist(&forged, &request("read_only"), &mut adapter)
                .unwrap_err()
                .code,
            "RUN_STATE_CONFLICT"
        );
        let mut denied = request("read_only");
        denied.role_ref = "not-installed".to_owned();
        assert_eq!(
            store
                .request_specialist(
                    &context(session.session_id, "denied"),
                    &denied,
                    &mut adapter
                )
                .unwrap_err()
                .code,
            "SPECIALIST_POLICY_DENIED"
        );
        assert!(adapter.calls.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn role_target_selection_prefers_idle_then_lower_run_identity() {
        let root = root();
        let (mut store, session) = active_session(&root, "fully_delegated");
        let mut adapter = FakeAdapter::default();
        let first = store
            .request_specialist(
                &context(session.session_id, "first-member"),
                &request("read_only"),
                &mut adapter,
            )
            .unwrap()
            .specialist_run_id
            .unwrap();
        let second = store
            .request_specialist(
                &context(session.session_id, "second-member"),
                &request("read_only"),
                &mut adapter,
            )
            .unwrap()
            .specialist_run_id
            .unwrap();
        let (lower, higher) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        let selector = TargetSelector::Role {
            role_ref: "reviewer".to_owned(),
        };
        assert_eq!(
            store.resolve_target(session.session_id, &selector).unwrap(),
            lower
        );
        store
            .assign_task(
                &context(session.session_id, "busy-lower"),
                &AssignSpecialistTask {
                    specialist_run_id: lower,
                    objective: "Retain one pending result.".to_owned(),
                    context_refs: Vec::new(),
                    expected_output: vec!["Result".to_owned()],
                    requested_access: "read_only".to_owned(),
                    deadline_seconds: 60,
                },
                &mut adapter,
            )
            .unwrap();
        assert_eq!(
            store.resolve_target(session.session_id, &selector).unwrap(),
            higher
        );
        store.collect_results(session.session_id, 0, 1).unwrap();
        assert_eq!(
            store.resolve_target(session.session_id, &selector).unwrap(),
            lower
        );
        store
            .finish_session(session.session_id, false, &mut adapter)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reuse_policy_distinguishes_idle_from_busy_compatible_members() {
        for (reuse_policy, expects_reuse_while_busy) in [
            ("reuse_idle_compatible", false),
            ("reuse_any_compatible", true),
        ] {
            let root = root();
            let mut configured = policy("fully_delegated");
            configured.roles[0].reuse_policy = reuse_policy.to_owned();
            let (mut store, session) = active_session_with_policy(&root, configured);
            let mut adapter = FakeAdapter::default();
            let first = store
                .request_specialist(
                    &context(session.session_id, "initial-member"),
                    &request("read_only"),
                    &mut adapter,
                )
                .unwrap();
            let first_run = first.specialist_run_id.unwrap();
            store
                .assign_task(
                    &context(session.session_id, "pending-result"),
                    &AssignSpecialistTask {
                        specialist_run_id: first_run,
                        objective: "Keep one result pending delivery.".to_owned(),
                        context_refs: Vec::new(),
                        expected_output: vec!["Result".to_owned()],
                        requested_access: "read_only".to_owned(),
                        deadline_seconds: 60,
                    },
                    &mut adapter,
                )
                .unwrap();
            let while_busy = store
                .request_specialist(
                    &context(session.session_id, "reuse-while-busy"),
                    &request("read_only"),
                    &mut adapter,
                )
                .unwrap();
            assert_eq!(while_busy.reused, expects_reuse_while_busy);
            if expects_reuse_while_busy {
                assert_eq!(while_busy.specialist_run_id, Some(first_run));
            } else {
                assert_ne!(while_busy.specialist_run_id, Some(first_run));
            }
            store.collect_results(session.session_id, 0, 1).unwrap();
            let after_delivery = store
                .request_specialist(
                    &context(session.session_id, "reuse-after-delivery"),
                    &request("read_only"),
                    &mut adapter,
                )
                .unwrap();
            assert!(after_delivery.reused);
            store
                .finish_session(session.session_id, false, &mut adapter)
                .unwrap();
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn reuse_receipt_survives_lost_tool_result_and_queue_state_change() {
        let root = root();
        let mut configured = policy("fully_delegated");
        configured.roles[0].reuse_policy = "reuse_idle_compatible".to_owned();
        let (mut store, session) = active_session_with_policy(&root, configured);
        let mut adapter = FakeAdapter::default();
        let initial = store
            .request_specialist(
                &context(session.session_id, "initial-member"),
                &request("read_only"),
                &mut adapter,
            )
            .unwrap();
        let initial_run = initial.specialist_run_id.unwrap();
        let request_context = context(session.session_id, "lost-reuse-response");
        let request_payload = serde_json::json!({
            "operation":"request_specialist",
            "role_ref":"reviewer",
            "objective":"Review the implementation.",
            "expected_output":["A bounded verdict"],
            "requested_access":"read_only",
            "deadline_seconds":60,
        });
        let lost_response = {
            let mut service = PrimaryOrchestrationService {
                store: &mut store,
                adapter: &mut adapter,
            };
            service
                .execute(
                    &request_context,
                    ToolRequest::RequestSpecialist {
                        role_ref: "reviewer".to_owned(),
                        objective: "Review the implementation.".to_owned(),
                        expected_output: vec!["A bounded verdict".to_owned()],
                        requested_access: "read_only".to_owned(),
                        deadline_seconds: 60,
                    },
                )
                .unwrap()
        };
        assert_eq!(lost_response["specialist_operation"]["reused"], true);
        assert_eq!(
            lost_response["specialist_operation"]["specialist_run_id"],
            initial_run.to_string()
        );
        let mut changed_request = request("read_only");
        changed_request.objective.push_str(" Changed.");
        assert_eq!(
            store
                .request_specialist(&request_context, &changed_request, &mut adapter)
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        drop(store);

        let mut store = OrchestrationStore::open(&root).unwrap();
        store
            .assign_task(
                &context(session.session_id, "make-original-busy"),
                &task_request(initial_run, "read_only"),
                &mut adapter,
            )
            .unwrap();
        let replay = PrimaryOrchestrationService {
            store: &mut store,
            adapter: &mut adapter,
        }
        .dispatch(&request_context, &request_payload)
        .unwrap();
        assert_eq!(replay, lost_response);
        assert_eq!(store.members(session.session_id).unwrap().len(), 1);
        store.collect_results(session.session_id, 0, 1).unwrap();
        store
            .finish_session(session.session_id, false, &mut adapter)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tool_dispatch_covers_checked_surface_and_replays_exact_bound_call() {
        let root = root();
        let (mut store, session) = active_session(&root, "fully_delegated");
        let mut adapter = FakeAdapter::default();

        let request_payload = serde_json::json!({
            "operation":"request_specialist",
            "role_ref":"reviewer",
            "objective":"Review the implementation.",
            "expected_output":["A bounded verdict"],
            "requested_access":"read_only",
            "deadline_seconds":60,
        });
        let request_context = context(session.session_id, "dispatch-request");
        let mut service = PrimaryOrchestrationService {
            store: &mut store,
            adapter: &mut adapter,
        };
        let requested = service
            .dispatch(&request_context, &request_payload)
            .unwrap();
        assert_eq!(requested["operation"], "request_specialist_result");
        let operation_id: Uuid =
            serde_json::from_value(requested["specialist_operation"]["operation_id"].clone())
                .unwrap();
        let child: Uuid =
            serde_json::from_value(requested["specialist_operation"]["specialist_run_id"].clone())
                .unwrap();
        let adapter_call_count = service.adapter.calls.len();
        assert_eq!(
            service
                .dispatch(&request_context, &request_payload)
                .unwrap(),
            requested
        );
        assert_eq!(service.adapter.calls.len(), adapter_call_count);

        let mut rebound = request_context.clone();
        rebound.source_turn_id = "turn-forged".to_owned();
        assert_eq!(
            service
                .dispatch(&rebound, &request_payload)
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        let mut forged = context(session.session_id, "forged-source");
        forged.source_run_id = new_uuid_v7();
        assert_eq!(
            service
                .dispatch(
                    &forged,
                    &serde_json::json!({"operation":"list_specialists"})
                )
                .unwrap_err()
                .code,
            "RUN_STATE_CONFLICT"
        );
        assert_eq!(
            service
                .dispatch(
                    &context(session.session_id, "source-injection"),
                    &serde_json::json!({
                        "operation":"list_specialists",
                        "session_id":session.session_id,
                    }),
                )
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );

        let awaited = service
            .dispatch(
                &context(session.session_id, "dispatch-await-operation"),
                &serde_json::json!({
                    "operation":"await_specialist_operations",
                    "operation_ids":[operation_id],
                    "return_when":"all",
                    "transport_wait_seconds":1,
                }),
            )
            .unwrap();
        assert_eq!(awaited["operation"], "await_specialist_operations_result");
        assert_eq!(awaited["pending"], serde_json::json!([]));

        let listed = service
            .dispatch(
                &context(session.session_id, "dispatch-list"),
                &serde_json::json!({"operation":"list_specialists"}),
            )
            .unwrap();
        assert_eq!(listed["operation"], "list_specialists_result");
        assert_eq!(listed["specialists"][0]["run_id"], child.to_string());

        let assigned = service
            .dispatch(
                &context(session.session_id, "dispatch-assign"),
                &serde_json::json!({
                    "operation":"assign_specialist_task",
                    "target":{"run_id":child},
                    "objective":"Return the review verdict.",
                    "context_refs":[],
                    "expected_output":["One verdict"],
                    "execution_intent":"read_only",
                    "blocking":false,
                    "deadline_seconds":60,
                }),
            )
            .unwrap();
        assert_eq!(assigned["operation"], "assign_specialist_task_result");
        let task_id: Uuid = serde_json::from_value(assigned["task_id"].clone()).unwrap();

        let task_wait = service
            .dispatch(
                &context(session.session_id, "dispatch-await-task"),
                &serde_json::json!({
                    "operation":"await_specialist_tasks",
                    "task_ids":[task_id],
                    "return_when":"all",
                    "transport_wait_seconds":1,
                }),
            )
            .unwrap();
        assert_eq!(task_wait["operation"], "await_specialist_tasks_result");
        assert_eq!(task_wait["tasks"][0]["state"], "completed");

        let collected = service
            .dispatch(
                &context(session.session_id, "dispatch-collect"),
                &serde_json::json!({
                    "operation":"collect_specialist_results",
                    "limit":8,
                }),
            )
            .unwrap();
        assert_eq!(collected["operation"], "collect_specialist_results_result");
        assert_eq!(collected["tasks"][0]["task_id"], task_id.to_string());

        let cancelled = service
            .dispatch(
                &context(session.session_id, "dispatch-cancel"),
                &serde_json::json!({
                    "operation":"cancel_specialist_task",
                    "task_id":task_id,
                    "reason":"No further work is required.",
                }),
            )
            .unwrap();
        assert_eq!(cancelled["operation"], "cancel_specialist_task_result");
        assert_eq!(cancelled["state"], "already_terminal");

        let released = service
            .dispatch(
                &context(session.session_id, "dispatch-release"),
                &serde_json::json!({
                    "operation":"release_specialist",
                    "run_id":child,
                    "reason":"The bounded task is complete.",
                }),
            )
            .unwrap();
        assert_eq!(released["operation"], "release_specialist_result");
        assert_eq!(released["state"], "retired");
        std::fs::remove_dir_all(root).unwrap();
    }
}
