//! Durable External Specialist Engagement authority.

use crate::controller::{CredentialCarrier, authorize_controller};
use crate::delegation_identity::{AcceptedSubjects, LookupNamespace, OperationReceipt};
use crate::global_runtime::GlobalProfileBinding;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use crate::review_output::{ReviewExecution, ReviewOutputDiagnostic, attach_execution, diagnostic};
use crate::run::{
    AgentConfigurationSnapshot, AggregateBinding, ControllerBinding, agent_configuration_digest,
};
use crate::specialist::{
    ReviewerRuntimePlan, validate_reviewer_output, validate_reviewer_output_v3,
};
use crate::task_request::{STRUCTURED_REVIEW_OUTPUT, SpecialistTaskRequest};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 6;

/// The exact accepted base tables of workspace Engagement database generation 6.
///
/// This is the single DDL source for generation 6: the fresh bootstrap and the
/// `5 -> 6` migration both execute it, so a new workspace and a migrated one
/// cannot drift. `metadata.orchestration_schema_version` stays `3`.
///
/// Every statement is strict: no object is created `IF NOT EXISTS`. A database
/// that claims an older generation while already carrying any generation-6
/// object is therefore not a verified shape, and the owning transaction rolls
/// back with a typed failure instead of silently converging on a schema that
/// only looks migrated.
const GENERATION_SIX_DDL: &str = "
CREATE TABLE delegation_operations(
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  operation_ref TEXT NOT NULL UNIQUE,
  schema_version INTEGER NOT NULL DEFAULT 2,
  state TEXT NOT NULL,
  workspace_ref TEXT NOT NULL,
  controller_principal TEXT NOT NULL,
  controller_generation INTEGER NOT NULL,
  scope_kind TEXT,
  scope_ref TEXT,
  operation_kind TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  mutation_envelope_json TEXT NOT NULL,
  mutation_envelope_digest TEXT NOT NULL,
  expected_revision INTEGER,
  trusted_invocation_json TEXT,
  accepted_subjects_json TEXT NOT NULL,
  receipt_json TEXT,
  definitive_refusal TEXT,
  refusal_revision INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  CHECK(schema_version = 2),
  CHECK(state IN ('prepared','accepted','resolved_not_accepted','unresolved')),
  CHECK(controller_generation BETWEEN 1 AND 9007199254740991),
  CHECK((scope_kind IS NULL) = (scope_ref IS NULL)),
  CHECK(CASE state
    WHEN 'prepared' THEN receipt_json IS NULL AND definitive_refusal IS NULL
      AND refusal_revision IS NULL
    WHEN 'unresolved' THEN receipt_json IS NULL AND definitive_refusal IS NULL
      AND refusal_revision IS NULL
    WHEN 'accepted' THEN receipt_json IS NOT NULL AND definitive_refusal IS NULL
      AND refusal_revision IS NULL
    WHEN 'resolved_not_accepted' THEN receipt_json IS NULL
      AND definitive_refusal IS NOT NULL AND refusal_revision IS NOT NULL
    ELSE 0 END)
) STRICT;
CREATE UNIQUE INDEX delegation_key_scoped ON delegation_operations(
  workspace_ref, controller_principal, controller_generation,
  scope_kind, scope_ref, operation_kind, idempotency_key)
  WHERE scope_kind IS NOT NULL AND state <> 'resolved_not_accepted';
CREATE UNIQUE INDEX delegation_key_prealloc ON delegation_operations(
  workspace_ref, controller_principal, controller_generation, operation_kind, idempotency_key)
  WHERE scope_kind IS NULL AND state <> 'resolved_not_accepted';
CREATE TRIGGER delegation_identity_fixed BEFORE UPDATE ON delegation_operations
WHEN NEW.operation_ref IS NOT OLD.operation_ref
  OR NEW.schema_version IS NOT OLD.schema_version
  OR NEW.workspace_ref IS NOT OLD.workspace_ref
  OR NEW.controller_principal IS NOT OLD.controller_principal
  OR NEW.controller_generation IS NOT OLD.controller_generation
  OR NEW.scope_kind IS NOT OLD.scope_kind OR NEW.scope_ref IS NOT OLD.scope_ref
  OR NEW.operation_kind IS NOT OLD.operation_kind
  OR NEW.idempotency_key IS NOT OLD.idempotency_key
  OR NEW.mutation_envelope_json IS NOT OLD.mutation_envelope_json
  OR NEW.mutation_envelope_digest IS NOT OLD.mutation_envelope_digest
BEGIN SELECT RAISE(ABORT,'operation identity changed'); END;
CREATE TRIGGER delegation_receipt_frozen BEFORE UPDATE ON delegation_operations
WHEN NOT(
  (OLD.receipt_json IS NULL AND NEW.receipt_json IS NOT NULL
     AND NEW.state = 'accepted' AND OLD.state IN ('prepared','unresolved'))
  OR (OLD.definitive_refusal IS NULL AND NEW.definitive_refusal IS NOT NULL
     AND NEW.refusal_revision IS NOT NULL
     AND NEW.state = 'resolved_not_accepted' AND OLD.state IN ('prepared','unresolved'))
  OR (OLD.receipt_json IS NEW.receipt_json
     AND OLD.definitive_refusal IS NEW.definitive_refusal
     AND OLD.refusal_revision IS NEW.refusal_revision))
BEGIN SELECT RAISE(ABORT,'operation receipt is immutable'); END;
CREATE TRIGGER delegation_state_transition BEFORE UPDATE OF state ON delegation_operations
WHEN NOT(OLD.state = NEW.state
  OR (OLD.state = 'prepared' AND NEW.state IN ('accepted','unresolved','resolved_not_accepted'))
  OR (OLD.state = 'unresolved' AND NEW.state IN ('accepted','resolved_not_accepted')))
BEGIN SELECT RAISE(ABORT,'operation state transition'); END;
CREATE TABLE caller_scope_assignments(
  assignment_id TEXT PRIMARY KEY,
  binding_ref TEXT NOT NULL,
  podway_workspace_ref TEXT NOT NULL,
  podway_session_ref TEXT NOT NULL,
  podway_attempt_ref TEXT NOT NULL,
  delegated_placement_ref TEXT NOT NULL,
  workspace_ref TEXT NOT NULL,
  aggregate_scope_kind TEXT NOT NULL,
  aggregate_scope_ref TEXT NOT NULL,
  member_ref TEXT NOT NULL,
  max_rounds INTEGER NOT NULL,
  accounting_state TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  CHECK(max_rounds BETWEEN 1 AND 32),
  CHECK(accounting_state IN ('counted_unresolved','counted_accepted','proved_nondispatch'))
) STRICT;
CREATE INDEX caller_scope_namespace ON caller_scope_assignments(
  binding_ref, podway_workspace_ref, podway_session_ref, podway_attempt_ref,
  delegated_placement_ref);
CREATE TRIGGER assignment_identity_fixed BEFORE UPDATE ON caller_scope_assignments
WHEN NEW.assignment_id IS NOT OLD.assignment_id
  OR NEW.binding_ref IS NOT OLD.binding_ref
  OR NEW.podway_workspace_ref IS NOT OLD.podway_workspace_ref
  OR NEW.podway_session_ref IS NOT OLD.podway_session_ref
  OR NEW.podway_attempt_ref IS NOT OLD.podway_attempt_ref
  OR NEW.delegated_placement_ref IS NOT OLD.delegated_placement_ref
  OR NEW.workspace_ref IS NOT OLD.workspace_ref
  OR NEW.aggregate_scope_kind IS NOT OLD.aggregate_scope_kind
  OR NEW.aggregate_scope_ref IS NOT OLD.aggregate_scope_ref
  OR NEW.member_ref IS NOT OLD.member_ref
  OR NEW.max_rounds IS NOT OLD.max_rounds
  OR NEW.created_at_ms IS NOT OLD.created_at_ms
BEGIN SELECT RAISE(ABORT,'caller assignment identity changed'); END;
CREATE TRIGGER caller_scope_budget_fixed BEFORE INSERT ON caller_scope_assignments
WHEN EXISTS(
  SELECT 1 FROM caller_scope_assignments a
   WHERE a.binding_ref = NEW.binding_ref
     AND a.podway_workspace_ref = NEW.podway_workspace_ref
     AND a.podway_session_ref = NEW.podway_session_ref
     AND a.podway_attempt_ref = NEW.podway_attempt_ref
     AND a.delegated_placement_ref = NEW.delegated_placement_ref
     AND a.max_rounds <> NEW.max_rounds)
BEGIN SELECT RAISE(ABORT,'caller scope budget changed'); END;
CREATE TRIGGER accounting_transition BEFORE UPDATE OF accounting_state
ON caller_scope_assignments
WHEN NOT(OLD.accounting_state = NEW.accounting_state
  OR (OLD.accounting_state = 'counted_unresolved'
      AND NEW.accounting_state IN ('counted_accepted','proved_nondispatch')))
BEGIN SELECT RAISE(ABORT,'caller assignment accounting transition'); END;
CREATE TABLE scope_membership_revisions(
  workspace_ref TEXT NOT NULL,
  scope_kind TEXT NOT NULL,
  scope_ref TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(workspace_ref, scope_kind, scope_ref),
  CHECK(scope_kind IN ('external_engagement','orchestrated_session')),
  CHECK(revision BETWEEN 0 AND 9007199254740991)
) STRICT;
CREATE TRIGGER scope_membership_monotonic BEFORE UPDATE ON scope_membership_revisions
WHEN NEW.revision < OLD.revision
BEGIN SELECT RAISE(ABORT,'scope revision decreased'); END;
CREATE TABLE member_workload_revisions(
  workspace_ref TEXT NOT NULL,
  scope_kind TEXT NOT NULL,
  scope_ref TEXT NOT NULL,
  member_ref TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(workspace_ref, scope_kind, scope_ref, member_ref),
  CHECK(scope_kind IN ('external_engagement','orchestrated_session')),
  CHECK(revision BETWEEN 0 AND 9007199254740991)
) STRICT;
CREATE TRIGGER member_workload_monotonic BEFORE UPDATE ON member_workload_revisions
WHEN NEW.revision < OLD.revision
BEGIN SELECT RAISE(ABORT,'member workload revision decreased'); END;
CREATE TABLE member_release_operations(
  operation_ref TEXT PRIMARY KEY,
  operation_kind TEXT NOT NULL,
  workspace_ref TEXT NOT NULL,
  scope_kind TEXT NOT NULL,
  scope_ref TEXT NOT NULL,
  member_ref TEXT NOT NULL,
  run_ref TEXT NOT NULL,
  controller_principal TEXT NOT NULL,
  controller_generation INTEGER NOT NULL,
  admitted_membership_revision INTEGER NOT NULL,
  owner_ref TEXT,
  progress TEXT NOT NULL,
  safe_failure_code TEXT,
  required_action TEXT NOT NULL,
  terminal_membership_revision INTEGER,
  settled_workload_revision INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(workspace_ref, scope_kind, scope_ref, member_ref),
  CHECK((operation_kind = 'release_external_specialist'
         AND scope_kind = 'external_engagement')
     OR (operation_kind = 'release_specialist' AND scope_kind = 'orchestrated_session')),
  CHECK(controller_generation BETWEEN 1 AND 9007199254740991),
  CHECK(admitted_membership_revision BETWEEN 0 AND 9007199254740991),
  CHECK(CASE progress
    WHEN 'requested' THEN terminal_membership_revision IS NULL
      AND settled_workload_revision IS NULL AND safe_failure_code IS NULL
      AND required_action = 'none'
    WHEN 'settling' THEN terminal_membership_revision IS NULL
      AND settled_workload_revision IS NULL AND safe_failure_code IS NULL
      AND required_action = 'none'
    WHEN 'terminal' THEN terminal_membership_revision IS NOT NULL
      AND settled_workload_revision IS NOT NULL AND safe_failure_code IS NULL
      AND required_action = 'none'
      AND terminal_membership_revision > admitted_membership_revision
      AND terminal_membership_revision BETWEEN 0 AND 9007199254740991
      AND settled_workload_revision BETWEEN 0 AND 9007199254740991
    WHEN 'cleanup_failed' THEN terminal_membership_revision IS NULL
      AND settled_workload_revision IS NULL AND safe_failure_code IS NOT NULL
      AND required_action IN ('reconcile_operation','recover_run','restore_state')
    WHEN 'recovery_needed' THEN terminal_membership_revision IS NULL
      AND settled_workload_revision IS NULL AND safe_failure_code IS NOT NULL
      AND required_action IN ('reconcile_operation','recover_run','restore_state')
    ELSE 0 END)
) STRICT;
CREATE TRIGGER release_admission_immutable BEFORE UPDATE ON member_release_operations
WHEN NEW.operation_ref IS NOT OLD.operation_ref
  OR NEW.operation_kind IS NOT OLD.operation_kind
  OR NEW.workspace_ref IS NOT OLD.workspace_ref
  OR NEW.scope_kind IS NOT OLD.scope_kind OR NEW.scope_ref IS NOT OLD.scope_ref
  OR NEW.member_ref IS NOT OLD.member_ref OR NEW.run_ref IS NOT OLD.run_ref
  OR NEW.controller_principal IS NOT OLD.controller_principal
  OR NEW.controller_generation IS NOT OLD.controller_generation
  OR NEW.admitted_membership_revision IS NOT OLD.admitted_membership_revision
BEGIN SELECT RAISE(ABORT,'release admission changed'); END;
CREATE TRIGGER release_terminal_frozen BEFORE UPDATE ON member_release_operations
WHEN OLD.progress = 'terminal'
  AND (NEW.progress IS NOT OLD.progress
       OR NEW.terminal_membership_revision IS NOT OLD.terminal_membership_revision
       OR NEW.settled_workload_revision IS NOT OLD.settled_workload_revision)
BEGIN SELECT RAISE(ABORT,'settled release is immutable'); END;
CREATE TABLE dispatch_indexes(
  task_id TEXT PRIMARY KEY,
  workspace_ref TEXT NOT NULL,
  run_ref TEXT NOT NULL,
  head_commit TEXT,
  index_sha256 TEXT NOT NULL,
  index_byte_length INTEGER NOT NULL,
  file_count INTEGER NOT NULL,
  baseline_bytes INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  CHECK(length(index_sha256) = 64 AND index_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK(file_count BETWEEN 0 AND 10000),
  CHECK(index_byte_length BETWEEN 1 AND 67108864),
  CHECK(baseline_bytes BETWEEN 0 AND 268435456)
) STRICT;
CREATE INDEX dispatch_indexes_by_run ON dispatch_indexes(run_ref);
CREATE TRIGGER dispatch_index_fixed BEFORE UPDATE ON dispatch_indexes
BEGIN SELECT RAISE(ABORT,'dispatch index is immutable'); END;
";

/// The file that proves which namespace this workspace's Engagement database
/// belongs to. Created by whichever path creates the database and republished
/// from the database by the next writable open when it is missing.
pub const NAMESPACE_BIRTH_FILE: &str = "namespace-birth.json";
const NAMESPACE_BIRTH_SCHEMA: &str = "dolgorae.namespace-birth/v1";
/// The SHA-256 of zero bytes, i.e. the digest of the empty provenance.
///
/// The v5 successor open request carries no external controller reference, so
/// its bootstrap event records the digest of that empty provenance instead of
/// guessing a synthetic reference. Legacy opens always record their real one.
const NO_EXTERNAL_PROVENANCE_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// The per-Run aggregate bound over retained dispatch-index baseline bytes and
/// retained change content together.
pub(crate) const MAX_RUN_BASELINE_BYTES: u64 = 268_435_456;

/// The content-store owner kind of one retained change side.
///
/// The owner is the accepted task, and the representation is `before:<path>` or
/// `after:<path>`, so a retained side names its task, its path and its side.
const SUCCESSOR_CHANGE_KIND: &str = "successor_change";

/// Retain the exact bytes one `ChangeRef` side names.
///
/// The reference is written in the caller's transaction so the settlement and its
/// retained content commit together, and identical bytes are stored once because
/// the content row is keyed by their digest.
pub(crate) fn retain_change_side(
    transaction: &Transaction<'_>,
    task_id: Uuid,
    path: &str,
    side: &str,
    bytes: &[u8],
) -> Result<(), MachineError> {
    crate::storage_observation::put_content(
        transaction,
        SUCCESSOR_CHANGE_KIND,
        &task_id.to_string(),
        &format!("{side}:{path}"),
        bytes,
    )
    .map(|_| ())
}

/// The bytes one retained change side names, if it is retained.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn retained_change_side(
    connection: &Connection,
    task_id: Uuid,
    path: &str,
    side: &str,
) -> Result<Option<Vec<u8>>, MachineError> {
    crate::storage_observation::read_content(
        connection,
        SUCCESSOR_CHANGE_KIND,
        &task_id.to_string(),
        &format!("{side}:{path}"),
    )
}

/// The bytes one Run retains under its per-Run aggregate.
///
/// The dispatch index retains the bytes of every dirty and untracked path at
/// dispatch; a settlement additionally retains the complete before and after bytes
/// its changes name. Both count against one per-Run bound, so a Run cannot exhaust
/// storage twice over.
pub(crate) fn run_retained_bytes(
    connection: &Connection,
    run_ref: Uuid,
) -> Result<u64, MachineError> {
    let baseline: u64 = connection
        .query_row(
            "SELECT COALESCE(SUM(baseline_bytes),0) FROM dispatch_indexes WHERE run_ref=?1",
            [run_ref.to_string()],
            |row| row.get(0),
        )
        .map_err(internal)?;
    let changes: u64 = connection
        .query_row(
            "SELECT COALESCE(SUM(r.byte_length),0)
               FROM result_content_refs r
               JOIN dispatch_indexes d ON d.task_id = r.owner_id
              WHERE r.owner_kind=?1 AND d.run_ref=?2",
            params![SUCCESSOR_CHANGE_KIND, run_ref.to_string()],
            |row| row.get(0),
        )
        .map_err(internal)?;
    baseline
        .checked_add(changes)
        .ok_or_else(|| internal("the Run's retained bytes are unrepresentable"))
}

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
    BeforeMigrationV6Commit,
    AfterMigrationV6Commit,
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
    diagnostic: Option<&'a ReviewOutputDiagnostic>,
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

/// Private coordinator input. This is never a public inspection projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OneShotOperationInput {
    pub request_ref: Uuid,
    pub workspace_id: String,
    pub request: Value,
    pub profile: String,
    pub recovery_controller: ControllerBinding,
    pub carrier_root: PathBuf,
    pub temporary_server: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum OneShotTerminal {
    Succeeded { result: Value },
    Failed { safe_error_code: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OneShotOperation {
    pub schema: String,
    pub input: OneShotOperationInput,
    pub request_sha256: String,
    pub input_sha256: String,
    pub global_profile_binding: Option<GlobalProfileBinding>,
    pub capture_ref: Option<Uuid>,
    pub engagement_id: Option<Uuid>,
    pub terminal: Option<OneShotTerminal>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OneShotTaskSnapshot {
    pub task_id: Uuid,
    pub specialist_run_id: Uuid,
    pub state: String,
    pub safe_error_code: Option<String>,
    pub diagnostic: Option<ReviewOutputDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OneShotObservation {
    /// Private receipt from the same SQLite snapshot as the relational facts.
    pub operation: OneShotOperation,
    pub engagement: Option<EngagementSnapshot>,
    pub task: Option<OneShotTaskSnapshot>,
    pub artifact: Option<CollectedReview>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<ReviewOutputDiagnostic>,
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
    connection: crate::storage_observation::CheckedConnection,
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
        // The pre-effect gate runs before any writable open, `chmod`, WAL pragma
        // or DDL. The generation probe runs first so an unsupported generation
        // always reports `ORCHESTRATION_SCHEMA_UNSUPPORTED`, even beside a corrupt
        // birth record (RPC:508-517). The record is then read as an
        // independent refusal: a record whose database is missing, empty or still
        // a legacy generation can never be repaired by creating or migrating a
        // database, so nothing is created.
        let probe = admit_generation(path)?;
        let record = read_birth_record(&birth_record_path(path)?)?;
        let Some(probe) = probe else {
            if record.is_some() {
                return Err(namespace_record_without_database());
            }
            return Self::create_fresh(path, faults);
        };
        match probe.schema_generation {
            AdmittedGeneration::Six => {
                let birth_id = probe_birth_id(&probe)?;
                drop(probe);
                if record.is_some_and(|record| record != birth_id) {
                    return Err(namespace_record_mismatch());
                }
                // Verification and the workspace lease are established before
                // the first writable effect, and the missing birth record is
                // published while that admission is still held.
                let mut connection =
                    crate::storage_observation::CheckedConnection::open(path, false)?;
                publish_birth_record(path, birth_id)?;
                crate::storage_observation::initialize_content(&mut connection)?;
                Ok(Self { connection, faults })
            }
            AdmittedGeneration::Legacy(generation) => {
                drop(probe);
                if record.is_some() {
                    return Err(namespace_record_mismatch());
                }
                Self::migrate_legacy(path, generation, faults)
            }
        }
    }

    /// Bootstrap one brand-new database and its namespace birth record.
    ///
    /// The historical bootstrap creates a valid generation 5 database and the
    /// single `5 -> 6` transaction takes it to the current generation. The
    /// admission lease is held from before the first write through publication,
    /// so a migration or birth never runs while workspace maintenance excludes
    /// database access.
    fn create_fresh(
        path: &Path,
        faults: Arc<dyn EngagementFaultInjector>,
    ) -> Result<Self, MachineError> {
        // Admission needs only the state root and its `workspace.json`, so it is
        // taken before the first filesystem effect: a workspace whose maintenance
        // lease excludes database access must not even gain a directory.
        let admission = crate::storage_observation::database_admission(path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(internal)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(internal)?;
        }
        let mut connection = Connection::open(path).map_err(internal)?;
        crate::storage_observation::install_trace(&connection)?;
        crate::storage_observation::count_migration();
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(internal)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(internal)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA synchronous=FULL;
                 BEGIN IMMEDIATE;
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
                   diagnostic_json TEXT,
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
                   artifact_id TEXT,
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
                 ) STRICT;
                 INSERT OR IGNORE INTO metadata(key,value) VALUES('schema_version','5');
                 COMMIT;",
            )
            .map_err(internal)?;
        // The historical bootstrap creates the generation-1 tables, so the
        // accepted chain completes generations 4 and 5 exactly as HEAD did, and
        // the generation-6 tables then come from the single DDL source in the
        // one transaction that also writes the namespace birth id.
        migrate_v3_to_v4(&mut connection)?;
        migrate_v4_to_v5(&mut connection)?;
        connection
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS one_active_external_task_per_member
                   ON tasks(specialist_run_id)
                   WHERE state IN ('accepted','queued','claimed','dispatching','running');",
            )
            .map_err(internal)?;
        let birth_id = fresh_birth_id();
        migrate_v5_to_v6(&mut connection, birth_id, &faults)?;
        let birth_id = database_birth_id(&connection)?;
        crate::storage_observation::initialize_content(&mut connection)?;
        drop(connection);
        publish_birth_record(path, birth_id)?;
        drop(admission);
        // Between dropping the bootstrap admission and this checked open the
        // database is a complete generation-6 namespace, so a maintenance owner
        // that wins the lease in that window sees a valid database and the
        // checked open either re-admits it or refuses with a typed busy error.
        let connection = crate::storage_observation::CheckedConnection::open(path, false)?;
        Ok(Self { connection, faults })
    }

    /// Migrate one existing legacy database directly into generation 6.
    ///
    /// Generation 5 and below already carry the base tables, so this path never
    /// re-runs the fresh bootstrap batch (no `CREATE TABLE`, `chmod` or WAL
    /// pragma before the owning migration). Verification and the workspace lease
    /// are established by the checked open before the first writable effect, and
    /// the same lease is retained as the store connection.
    fn migrate_legacy(
        path: &Path,
        generation: i64,
        faults: Arc<dyn EngagementFaultInjector>,
    ) -> Result<Self, MachineError> {
        let mut lease = crate::storage_observation::CheckedConnection::open(path, false)?;
        {
            let connection: &mut Connection = &mut lease;
            connection
                .execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")
                .map_err(internal)?;
            if generation == 1 {
                migrate_v1_to_v2(connection)?;
            }
            if generation <= 2 {
                migrate_v2_to_v3(connection)?;
            }
            migrate_v3_to_v4(connection)?;
            migrate_v4_to_v5(connection)?;
            let birth_id = fresh_birth_id();
            migrate_v5_to_v6(connection, birth_id, &faults)?;
            let birth_id = database_birth_id(connection)?;
            crate::storage_observation::initialize_content(connection)?;
            publish_birth_record(path, birth_id)?;
        }
        Ok(Self {
            connection: lease,
            faults,
        })
    }

    pub(crate) fn into_connection(self) -> crate::storage_observation::CheckedConnection {
        self.connection
    }

    /// Observation never creates storage, runs a migration, or replays an operation.
    pub(crate) fn open_read_only(path: &Path) -> Result<Self, MachineError> {
        if !path.is_absolute() {
            return Err(invalid(
                "database",
                "orchestration database path must be absolute",
            ));
        }
        // The same effect-free gate as the writable openers: an unsupported or
        // inconsistent generation fails before any connection is pooled. A
        // never-bootstrapped database is observationally identical to a missing
        // one, so it gets the same refusal.
        if admit_generation(path)?.is_none() {
            return Err(MachineError::new(
                "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                "orchestration database is not initialized",
                false,
                serde_json::json!({"observed":null}),
            ));
        }
        let connection = crate::storage_observation::CheckedConnection::open(path, true)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(internal)?;
        Ok(Self {
            connection,
            faults: Arc::new(NoEngagementFaults),
        })
    }

    pub(crate) fn reserve_one_shot(
        &mut self,
        input: &OneShotOperationInput,
    ) -> Result<(OneShotOperation, bool), MachineError> {
        validate_one_shot_input(input)?;
        let input_sha256 = digest_value(&serde_json::to_value(input).map_err(internal)?)?;
        let transaction = self.transaction()?;
        if let Some(existing) = one_shot_in(&transaction, input.request_ref)? {
            if existing.input != *input || existing.input_sha256 != input_sha256 {
                return Err(one_shot_conflict());
            }
            return Ok((existing, false));
        }
        let operation = OneShotOperation {
            schema: "dolgorae-one-shot-operation/v1".to_owned(),
            input: input.clone(),
            request_sha256: digest_value(&input.request)?,
            input_sha256,
            global_profile_binding: None,
            capture_ref: None,
            engagement_id: None,
            terminal: None,
        };
        let value = canonical_string(&serde_json::to_value(&operation).map_err(internal)?)?;
        transaction.execute(
            "INSERT INTO one_shot_operations(request_ref,receipt_json,receipt_sha256,engagement_id) VALUES(?1,?2,?3,NULL)",
            params![input.request_ref.to_string(), value, sha256_hex(value.as_bytes())],
        ).map_err(internal)?;
        crate::storage_observation::commit(transaction).map_err(internal)?;
        Ok((operation, true))
    }

    pub(crate) fn one_shot(
        &self,
        request_ref: Uuid,
    ) -> Result<Option<OneShotOperation>, MachineError> {
        one_shot_in(&self.connection, request_ref)
    }

    pub(crate) fn bind_one_shot_profile(
        &mut self,
        request_ref: Uuid,
        binding: &GlobalProfileBinding,
    ) -> Result<(), MachineError> {
        binding.validate_for_recovery()?;
        self.update_one_shot(request_ref, |operation| {
            if operation.input.profile != binding.selected_name {
                return Err(one_shot_conflict());
            }
            bind_once(&mut operation.global_profile_binding, binding.clone())
        })
    }

    /// Reservation is not proof that a capture was published.
    pub(crate) fn reserve_one_shot_capture(
        &mut self,
        request_ref: Uuid,
        capture_ref: Uuid,
    ) -> Result<(), MachineError> {
        if capture_ref.get_version_num() != 7 {
            return Err(invalid("capture_ref", "capture reference must be UUIDv7"));
        }
        self.update_one_shot(request_ref, |operation| {
            bind_once(&mut operation.capture_ref, capture_ref)
        })
    }

    pub(crate) fn finish_one_shot(
        &mut self,
        request_ref: Uuid,
        terminal: &OneShotTerminal,
    ) -> Result<(), MachineError> {
        match terminal {
            OneShotTerminal::Succeeded { result } if !result.is_object() => {
                return Err(invalid("result", "one-shot result must be an object"));
            }
            OneShotTerminal::Failed { safe_error_code }
                if !crate::machine::registered_errors().contains_key(safe_error_code.as_str()) =>
            {
                return Err(invalid(
                    "safe_error_code",
                    "one-shot error code must be registered",
                ));
            }
            _ => {}
        }
        self.update_one_shot(request_ref, |operation| {
            bind_once(&mut operation.terminal, terminal.clone())
        })
    }

    fn update_one_shot(
        &mut self,
        request_ref: Uuid,
        update: impl FnOnce(&mut OneShotOperation) -> Result<(), MachineError>,
    ) -> Result<(), MachineError> {
        let transaction = self.transaction()?;
        let mut operation = one_shot_in(&transaction, request_ref)?.ok_or_else(not_found)?;
        update(&mut operation)?;
        write_one_shot(&transaction, &operation)?;
        crate::storage_observation::commit(transaction).map_err(internal)
    }

    pub(crate) fn observe_one_shot(
        &self,
        request_ref: Uuid,
    ) -> Result<Option<OneShotObservation>, MachineError> {
        let transaction = self.connection.unchecked_transaction().map_err(internal)?;
        let Some(operation) = one_shot_in(&transaction, request_ref)? else {
            return Ok(None);
        };
        let mut observation = OneShotObservation {
            operation: operation.clone(),
            engagement: None,
            task: None,
            artifact: None,
        };
        if let Some(engagement_id) = operation.engagement_id {
            let (workspace, kind): (String, String) = transaction
                .query_row(
                    "SELECT workspace_id,authority_kind FROM engagements WHERE engagement_id=?1",
                    [engagement_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(internal)?;
            if workspace != operation.input.workspace_id || kind != "legacy_one_shot" {
                return Err(integrity("one-shot engagement binding does not match"));
            }
            let engagement = snapshot_in(&transaction, engagement_id)?;
            if let Some(task_id) = engagement.task_id {
                let (run_id, state, safe_error_code, diagnostic_json): (String, String, Option<String>, Option<String>) = transaction.query_row(
                    "SELECT specialist_run_id,state,safe_error_code,diagnostic_json FROM tasks WHERE task_id=?1 AND engagement_id=?2",
                    params![task_id.to_string(), engagement_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                ).map_err(internal)?;
                let specialist_run_id = run_id.parse().map_err(internal)?;
                if engagement.specialist_run_id != Some(specialist_run_id) {
                    return Err(integrity("one-shot task binding does not match"));
                }
                observation.task = Some(OneShotTaskSnapshot {
                    task_id,
                    specialist_run_id,
                    state,
                    safe_error_code,
                    diagnostic: diagnostic_json
                        .map(|value| serde_json::from_str(&value).map_err(internal))
                        .transpose()?,
                });
                // Legacy one-shot publication binds its sole artifact to the
                // engagement; task_id is populated by the reusable facade.
                let artifact: Option<(String, String)> = transaction.query_row(
                    "SELECT artifact_id,canonical_json FROM artifacts WHERE engagement_id=?2 AND (task_id=?1 OR task_id IS NULL)",
                    params![task_id.to_string(), engagement_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional().map_err(internal)?;
                observation.artifact = artifact
                    .map(|(id, output)| -> Result<CollectedReview, MachineError> {
                        Ok(CollectedReview {
                            artifact_id: id.parse().map_err(internal)?,
                            output: serde_json::from_str(&Self::resolve_artifact_json(
                                &transaction,
                                &id,
                                output,
                            )?)
                            .map_err(internal)?,
                        })
                    })
                    .transpose()?;
            }
            observation.engagement = Some(engagement);
        }
        Ok(Some(observation))
    }

    pub(crate) fn open_one_shot_engagement(
        &mut self,
        request_ref: Uuid,
        controller_sha256: &str,
        idempotency_key: &str,
    ) -> Result<EngagementSnapshot, MachineError> {
        let operation = self.one_shot(request_ref)?.ok_or_else(not_found)?;
        self.open_engagement_bound(
            &operation.input.workspace_id,
            controller_sha256,
            idempotency_key,
            Some(request_ref),
        )
    }

    pub fn open_engagement(
        &mut self,
        workspace_id: &str,
        controller_sha256: &str,
        idempotency_key: &str,
    ) -> Result<EngagementSnapshot, MachineError> {
        self.open_engagement_bound(workspace_id, controller_sha256, idempotency_key, None)
    }

    fn open_engagement_bound(
        &mut self,
        workspace_id: &str,
        controller_sha256: &str,
        idempotency_key: &str,
        request_ref: Option<Uuid>,
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
            if let Some(reference) = request_ref {
                let operation = self.one_shot(reference)?.ok_or_else(not_found)?;
                if operation.engagement_id != Some(replay.engagement_id) {
                    return Err(one_shot_conflict());
                }
            }
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
        if let Some(reference) = request_ref {
            let mut operation = one_shot_in(&transaction, reference)?.ok_or_else(not_found)?;
            if operation.terminal.is_some() {
                return Err(one_shot_conflict());
            }
            bind_once(&mut operation.engagement_id, engagement_id)?;
            write_one_shot(&transaction, &operation)?;
        }
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
                        diagnostic: None,
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
                diagnostic: None,
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
                        .ok_or_else(|| {
                            crate::review_output::invalid_output(
                                None,
                                None,
                                "output_unavailable",
                                "",
                            )
                        })
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
                        diagnostic: None,
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
                crate::storage_observation::put_content(
                    &transaction,
                    "external_artifact",
                    &artifact_id.to_string(),
                    "json",
                    canonical_json.as_bytes(),
                )?;
                crate::storage_observation::put_content(
                    &transaction,
                    "legacy_engagement",
                    &engagement_id.to_string(),
                    "json",
                    canonical_json.as_bytes(),
                )?;
                transaction
                    .execute(
                        "UPDATE engagements SET state='result_ready',result_sha256=?2,result_json=?3,revision=revision+1 WHERE engagement_id=?1",
                        params![engagement_id.to_string(), result_sha256, ""],
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
                            ""
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
                let mut terminal_error = execution.terminal_error;
                if let Some(error) = terminal_error.as_mut() {
                    attach_execution(
                        error,
                        ReviewExecution {
                            run_id: Some(specialist_run_id),
                            task_id: Some(reserved.task_id),
                            ..Default::default()
                        },
                    );
                }
                let final_result = TaskReservation {
                    state: "failed".to_owned(),
                    terminal_error,
                    ..reserved
                };
                let safe_error_code = final_result
                    .terminal_error
                    .as_ref()
                    .map(|error| error.code.as_str());
                let stored_diagnostic = final_result.terminal_error.as_ref().and_then(diagnostic);
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
                        diagnostic: stored_diagnostic.as_ref(),
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
                        diagnostic: None,
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
        let result = Self::resolve_artifact_json(
            &transaction,
            &artifact_id,
            result.ok_or_else(|| conflict("result artifact is missing"))?,
        )?;
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
            let observed = crate::storage_observation::epoch()?;
            self.connection.check_identity()?;
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
            crate::storage_observation::wait(
                observed,
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(250)),
            )?;
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
        require_external_owner_binding(binding)?;
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
        let result = open_external_engagement_in(
            &transaction,
            ExternalOpenCore {
                engagement_id,
                workspace_id,
                controller_sha256: &sha256_hex(binding_json.as_bytes()),
                bootstrap_operation_id,
                external_json: Some(&external_json),
                external_sha256: Some(&external_sha256),
                label,
                binding_json: &binding_json,
                now_ms: now,
            },
        )?;
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
        self.reserve_external_hire_with_provenance(
            engagement_id,
            role_ref,
            configuration,
            objective,
            requested_access,
            idempotency_key,
        )
        .map(|(reservation, _)| reservation)
    }

    /// Read an accepted configuration before consulting mutable Role sources.
    pub(crate) fn accepted_external_hire_configuration(
        &self,
        engagement_id: Uuid,
        key: &str,
    ) -> Result<Option<AgentConfigurationSnapshot>, MachineError> {
        let response: Option<String> = self.connection.query_row(
            "SELECT response_json FROM operations WHERE scope_id=?1 AND operation='hire_external_specialist' AND idempotency_key=?2",
            params![engagement_id.to_string(), key], |row| row.get(0)).optional().map_err(internal)?;
        let Some(response) = response else {
            return Ok(None);
        };
        let reservation: ExternalHireReservation =
            serde_json::from_str(&response).map_err(internal)?;
        if reservation.engagement_id != engagement_id {
            return Err(internal("hire receipt crossed engagement boundary"));
        }
        self.external_member_configuration(engagement_id, reservation.specialist_run_id)
            .map(|(configuration, _, _)| Some(configuration))
    }

    pub(crate) fn reserve_external_hire_with_provenance(
        &mut self,
        engagement_id: Uuid,
        role_ref: &str,
        configuration: &AgentConfigurationSnapshot,
        objective: &str,
        requested_access: &str,
        idempotency_key: &str,
    ) -> Result<(ExternalHireReservation, bool), MachineError> {
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
            return Ok((replay, false));
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
            return Ok((replay, false));
        }
        let result = reserve_external_hire_in(
            &transaction,
            ExternalHireReservationCore {
                engagement_id,
                hire_operation_id,
                specialist_run_id,
                role_ref,
                configuration_json: &configuration_json,
                configuration_sha256: &configuration_sha256,
                objective_sha256: Some(&objective_sha256),
                requested_access,
                event_payload: &request,
                now_ms: now,
            },
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
        Ok((result, true))
    }

    pub fn finish_external_hire(
        &mut self,
        reservation: &ExternalHireReservation,
        outcome: RuntimeOutcome,
        idempotency_key: &str,
    ) -> Result<ExternalHireReservation, MachineError> {
        let now = unix_time_ms()?;
        let faults = Arc::clone(&self.faults);
        let transaction = self.transaction()?;
        let Some(result) = settle_external_hire_in(&transaction, reservation, outcome, now)? else {
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
        };
        scoped_update_response(
            &transaction,
            &reservation.engagement_id.to_string(),
            "hire_external_specialist",
            idempotency_key,
            &result,
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

    pub(crate) fn is_reusable_member(
        &self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> Result<bool, MachineError> {
        let kind: String = self.connection.query_row(
            "SELECT e.authority_kind FROM members m JOIN engagements e ON e.engagement_id=m.engagement_id
             WHERE m.engagement_id=?1 AND m.specialist_run_id=?2",
            params![engagement_id.to_string(), specialist_run_id.to_string()],
            |row| row.get(0),
        ).optional().map_err(internal)?.ok_or_else(|| specialist_not_member(specialist_run_id))?;
        match kind.as_str() {
            "external_v1" => Ok(true),
            "legacy_one_shot" => Ok(false),
            _ => Err(integrity("unsupported engagement authority kind")),
        }
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

    /// The authenticated owner binding one external engagement recorded at open.
    ///
    /// Successor services verify this binding against the trusted Controller of
    /// the current call before they admit any keyed operation, so an aggregate
    /// has exactly the owner it was opened with (MDC:137).
    pub(crate) fn external_owner_binding(
        &self,
        engagement_id: Uuid,
    ) -> Result<(String, ControllerBinding), MachineError> {
        let row: Option<(String, Option<String>, String, String)> = self
            .connection
            .query_row(
                "SELECT workspace_id,controller_binding_json,controller_sha256,authority_kind
                 FROM engagements WHERE engagement_id=?1",
                [engagement_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(internal)?;
        let (workspace_id, binding_json, digest, kind) = row.ok_or_else(not_found)?;
        if kind != "external_v1" {
            return Err(not_found());
        }
        let binding_json = binding_json.ok_or_else(|| integrity("owner binding is missing"))?;
        if digest != sha256_hex(binding_json.as_bytes()) {
            return Err(integrity("owner binding digest does not match"));
        }
        let binding: ControllerBinding = serde_json::from_str(&binding_json)
            .map_err(|_| integrity("owner binding is invalid"))?;
        Ok((workspace_id, binding))
    }

    /// The accepted `install_provisioning` intent that retained one snapshot.
    ///
    /// A published snapshot is installed only when its own keyed install was
    /// accepted; a published-but-unaccepted snapshot never admits a member.
    pub(crate) fn accepted_install_receipt(
        &self,
        workspace_ref: &str,
        provisioning_ref: &str,
    ) -> Result<Option<OperationReceipt>, MachineError> {
        /// One retained install receipt row, in its stored column order.
        type InstallReceiptRow = (
            String,
            String,
            String,
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
            String,
            String,
        );
        let row: Option<InstallReceiptRow> = self
            .connection
            .query_row(
                "SELECT operation_ref,mutation_envelope_digest,receipt_json,accepted_subjects_json,
                        mutation_envelope_json,controller_principal,controller_generation,
                        scope_kind,scope_ref,operation_kind,idempotency_key
                   FROM delegation_operations
                  WHERE workspace_ref=?1 AND operation_kind='install_provisioning'
                    AND state='accepted'
                    AND json_extract(receipt_json,'$.accepted_subjects.provisioning_ref')=?2
                  ORDER BY sequence DESC LIMIT 1",
                params![workspace_ref, provisioning_ref],
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
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .optional()
            .map_err(internal)?;
        let Some((
            operation_ref,
            digest,
            receipt_json,
            subjects_json,
            envelope_json,
            principal,
            generation,
            scope_kind,
            scope_ref,
            operation_kind,
            idempotency_key,
        )) = row
        else {
            return Ok(None);
        };
        let namespace = LookupNamespace {
            controller_principal: principal,
            controller_generation: u64::try_from(generation)
                .map_err(|_| intent_corrupt("the retained controller generation is invalid"))?,
            workspace_ref: workspace_ref.to_owned(),
            scope_kind,
            scope_ref,
            operation_kind,
            idempotency_key,
        };
        intent_receipt(
            &namespace,
            intent_operation_ref(&operation_ref)?,
            &digest,
            &envelope_json,
            Some(&receipt_json),
            &subjects_json,
        )
        .map(Some)
    }

    /// The membership state and actor residency of one external member.
    ///
    /// Successor lookup derives a hire's current outcome from the member row,
    /// so it reads the same durable state the legacy facade exposes.
    pub(crate) fn external_member_state(
        &self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> Result<(String, String), MachineError> {
        self.connection
            .query_row(
                "SELECT state,COALESCE(actor_residency,'unavailable') FROM members
                 WHERE engagement_id=?1 AND specialist_run_id=?2",
                params![engagement_id.to_string(), specialist_run_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(|| specialist_not_member(specialist_run_id))
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
        let objective_sha256 = sha256_hex(objective.as_bytes());
        let result = reserve_external_task_in(
            &transaction,
            ExternalTaskReservationCore {
                engagement_id,
                specialist_run_id,
                task_id,
                objective_sha256: Some(&objective_sha256),
                external_request_json: Some(&external_json),
                request_json: &request_json,
                request_sha256: &request_sha256,
                execution_intent,
                deadline_seconds,
                now_ms: now,
            },
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
        self.finish_external_task_with_diagnostic(
            engagement_id,
            task_id,
            output,
            state,
            safe_error_code,
            None,
        )
    }

    pub fn finish_external_task_with_diagnostic(
        &mut self,
        engagement_id: Uuid,
        task_id: Uuid,
        output: Option<&Value>,
        state: &str,
        safe_error_code: Option<&str>,
        diagnostic: Option<&ReviewOutputDiagnostic>,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        if diagnostic.is_some()
            && (state != "failed" || safe_error_code != Some("REVIEW_OUTPUT_INVALID"))
        {
            return Err(invalid(
                "diagnostic",
                "diagnostics require terminal invalid review output",
            ));
        }
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
            crate::storage_observation::put_content(
                &transaction,
                "external_artifact",
                &artifact_id.to_string(),
                "json",
                canonical.as_bytes(),
            )?;
            transaction.execute(
                "INSERT INTO artifacts(artifact_id,engagement_id,task_id,specialist_run_id,result_sha256,canonical_json)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![artifact_id.to_string(),engagement_id.to_string(),task_id.to_string(),specialist,digest,""],
            ).map_err(internal)?;
            (Some(artifact_id), Some(digest))
        } else {
            (None, None)
        };
        let diagnostic_json = diagnostic
            .map(serde_json::to_string)
            .transpose()
            .map_err(internal)?;
        let Some(result) = terminalize_external_task_in(
            &transaction,
            TaskTerminal {
                engagement_id,
                task_id,
                state,
                safe_error_code,
                result_sha256: result_sha256.as_deref(),
                artifact_id,
                diagnostic_json: diagnostic_json.as_deref(),
                now_ms: now,
            },
        )?
        else {
            // The task was already terminal: nothing to commit.
            return external_task_in(&transaction, engagement_id, task_id);
        };
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

    pub(crate) fn external_task_for_key(
        &self,
        engagement_id: Uuid,
        key: &str,
    ) -> Result<ExternalTaskSnapshot, MachineError> {
        let task_id: String = self.connection.query_row(
            "SELECT json_extract(response_json,'$.task_id') FROM operations
             WHERE scope_id=?1 AND operation='assign_external_specialist_task' AND idempotency_key=?2",
            params![engagement_id.to_string(), key], |row| row.get(0),
        ).map_err(internal)?;
        let task_id = Uuid::parse_str(&task_id).map_err(internal)?;
        self.external_task_request(engagement_id, task_id)?;
        self.external_task(engagement_id, task_id)
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

    fn resolve_artifact_json(
        connection: &Connection,
        artifact: &str,
        stored: String,
    ) -> Result<String, MachineError> {
        let (expected, engagement_digest): (String, Option<String>) = connection.query_row(
            "SELECT a.result_sha256,e.result_sha256 FROM artifacts a JOIN engagements e ON e.engagement_id=a.engagement_id WHERE a.artifact_id=?1",
            [artifact], |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|_| integrity("original artifact result binding is absent"))?;
        if engagement_digest
            .as_deref()
            .is_some_and(|digest| digest != expected)
        {
            return Err(integrity("artifact and engagement result digests disagree"));
        }
        if !stored.is_empty() {
            if sha256_hex(stored.as_bytes()) != expected {
                return Err(integrity(
                    "legacy artifact JSON differs from its original digest",
                ));
            }
            return Ok(stored);
        }
        let bytes = crate::storage_observation::read_bound_content(
            connection,
            "external_artifact",
            artifact,
            "json",
            &expected,
        )?
        .ok_or_else(|| integrity("normalized artifact reference is absent"))?;
        String::from_utf8(bytes).map_err(internal)
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
             AND (state='completed_not_delivered' OR (state='failed' AND diagnostic_json IS NOT NULL))
             AND NOT EXISTS (SELECT 1 FROM delivery_receipts d WHERE d.task_id=tasks.task_id)
             ORDER BY created_at_ms,task_id LIMIT ?2",
            )
            .map_err(internal)?;
        let rows = pending
            .query_map(params![engagement_id.to_string(), remaining], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
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
                    "UPDATE tasks SET state='delivered' WHERE task_id=?1 AND state='completed_not_delivered'",
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
        // A successor member is released by its own release owner: while
        // one is unreleased the aggregate close refuses with the typed guard;
        // once every member is released the same close proceeds.
        {
            let mut statement = transaction
                .prepare(
                    "SELECT specialist_run_id FROM members
                      WHERE engagement_id=?1 AND state<>'released'",
                )
                .map_err(internal)?;
            let mut rows = statement
                .query(params![engagement_id.to_string()])
                .map_err(internal)?;
            let mut successor_unreleased = false;
            while let Some(row) = rows.next().map_err(internal)? {
                let member: String = row.get(0).map_err(internal)?;
                if successor_hire_member(&transaction, &member)? {
                    successor_unreleased = true;
                    break;
                }
            }
            drop(rows);
            drop(statement);
            if successor_unreleased {
                return Err(MachineError::new(
                    "SUCCESSOR_MEMBER_UNRELEASED",
                    "a successor member must be released by its own release owner",
                    false,
                    serde_json::json!({
                        "engagement_id":engagement_id,
                        "required_action":"reconcile_operation"
                    }),
                ));
            }
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
        let row: Option<(String, Option<String>, String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT hire_operation_id,role_ref,configuration_sha256,configuration_json FROM members
             WHERE engagement_id=?1 AND specialist_run_id=?2",
                params![engagement_id.to_string(), specialist_run_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(internal)?;
        let (operation, role, configuration, stored) =
            row.ok_or_else(|| specialist_not_member(specialist_run_id))?;
        let mut role_digest = role.as_ref().map(|value| sha256_hex(value.as_bytes()));
        if let Some(stored) = stored {
            let captured: AgentConfigurationSnapshot =
                serde_json::from_str(&stored).map_err(internal)?;
            if captured.schema_version == 3 {
                if agent_configuration_digest(&captured).map_err(internal)? != configuration {
                    return Err(integrity(
                        "stored member configuration digest does not match",
                    ));
                }
                role_digest = Some(
                    captured
                        .instructions
                        .caller()
                        .ok_or_else(|| integrity("caller member has no instruction capture"))?
                        .role
                        .sha256
                        .clone(),
                );
            } else if captured.schema_version == 4 {
                // A successor (delegated) member binds the closed role snapshot
                // digest; the aggregate binding carries its bare form.
                if agent_configuration_digest(&captured).map_err(internal)? != configuration {
                    return Err(integrity(
                        "stored member configuration digest does not match",
                    ));
                }
                role_digest = Some(
                    captured
                        .instructions
                        .delegation()
                        .ok_or_else(|| integrity("delegated member has no instruction capture"))?
                        .role_snapshot_digest
                        .trim_start_matches("sha256:")
                        .to_owned(),
                );
            }
        }
        if binding.operation_id.to_string() != operation
            || binding.role_reference != role
            || binding.role_snapshot_sha256 != role_digest
            || binding.agent_configuration_sha256.as_deref() != Some(configuration.as_str())
        {
            return Err(integrity(
                "specialist Run binding does not match its member record",
            ));
        }
        Ok(())
    }

    fn transaction(&mut self) -> Result<Transaction<'_>, MachineError> {
        self.connection.check_identity()?;
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(internal)
    }

    /// Run one successor transaction: it commits only if the closure succeeds.
    ///
    /// Successor keyed operations need their receipt and their state change in
    /// one transaction, so the store exposes the owning transaction instead of
    /// committing internally.
    pub(crate) fn transact<T>(
        &mut self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, MachineError>,
    ) -> Result<T, MachineError> {
        let transaction = self.transaction()?;
        let value = operation(&transaction)?;
        crate::storage_observation::commit(transaction).map_err(internal)?;
        Ok(value)
    }

    /// The store's verified connection for successor helpers that run their own
    /// transaction-scoped SQL (caller-scope accounting, retained task reads).
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The accepted successor assignment one assign key retained.
    ///
    /// A generation-4 member has no legacy `operations` receipt; its key maps to
    /// the accepted task through the shared intent store instead.
    pub(crate) fn external_task_for_successor_key(
        &self,
        workspace_ref: &str,
        engagement_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<Uuid>, MachineError> {
        let receipt: Option<String> = self
            .connection
            .query_row(
                "SELECT receipt_json FROM delegation_operations
                  WHERE workspace_ref=?1 AND scope_kind='external_engagement' AND scope_ref=?2
                    AND operation_kind='assign_external_specialist_task'
                    AND idempotency_key=?3 AND state='accepted'",
                params![workspace_ref, engagement_id.to_string(), idempotency_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?;
        let Some(receipt) = receipt else {
            return Ok(None);
        };
        let receipt: OperationReceipt = serde_json::from_str(&receipt)
            .map_err(|_| integrity("the retained receipt is invalid"))?;
        receipt
            .accepted_subjects
            .task_ref
            .as_deref()
            .map(|task| {
                Uuid::parse_str(task)
                    .map_err(|_| integrity("the retained task reference is invalid"))
            })
            .transpose()
    }

    /// The execution intent and retained deadline one member dispatch renders
    /// with.
    pub(crate) fn external_task_dispatch_facts(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<(String, u64), MachineError> {
        let row: Option<(Option<String>, Option<i64>)> = self
            .connection
            .query_row(
                "SELECT execution_intent,deadline_seconds FROM tasks
                  WHERE engagement_id=?1 AND task_id=?2",
                params![engagement_id.to_string(), task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((Some(intent), Some(deadline))) = row else {
            return Err(integrity("the retained dispatch facts are missing"));
        };
        Ok((
            intent,
            u64::try_from(deadline).map_err(|_| integrity("the retained deadline is invalid"))?,
        ))
    }

    /// The canonical acceptance body digest one accepted successor assignment
    /// froze in its immutable intent envelope.
    pub(crate) fn accepted_assignment_body_digest(
        &self,
        workspace_ref: &str,
        engagement_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<String>, MachineError> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT mutation_envelope_json,mutation_envelope_digest
                   FROM delegation_operations
                  WHERE workspace_ref=?1 AND scope_kind='external_engagement'
                    AND scope_ref=?2 AND operation_kind='assign_external_specialist_task'
                    AND idempotency_key=?3 AND state='accepted'",
                params![workspace_ref, engagement_id.to_string(), idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((envelope_json, digest)) = row else {
            return Ok(None);
        };
        let parsed = crate::jcs::parse(&envelope_json)
            .map_err(|_| integrity("the retained mutation envelope is not canonical JSON"))?;
        let canonical = crate::jcs::canonicalize(&parsed)
            .map_err(|_| integrity("the retained mutation envelope cannot be re-encoded"))?;
        let envelope: crate::operation::Envelope = serde_json::from_slice(&canonical)
            .map_err(|_| integrity("the retained mutation envelope is not the closed envelope"))?;
        if crate::operation::envelope_digest(&envelope)
            .map_err(|_| integrity("the retained mutation envelope cannot be re-digested"))?
            != digest
        {
            return Err(integrity(
                "the retained mutation envelope differs from its intent row",
            ));
        }
        Ok(Some(envelope.request_body.sha256))
    }

    /// Every unsettled task of one member, with the Turn it is bound to.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn unsettled_member_tasks(
        &self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> Result<Vec<(Uuid, String, String)>, MachineError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT task_id,execution_intent,turn_id FROM tasks
                  WHERE engagement_id=?1 AND specialist_run_id=?2
                    AND state IN ('dispatching','running') AND turn_id IS NOT NULL
                  ORDER BY created_at_ms",
            )
            .map_err(internal)?;
        let rows = statement
            .query_map(
                params![engagement_id.to_string(), specialist_run_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)?;
        let mut tasks = Vec::with_capacity(rows.len());
        for (task, intent, turn) in rows {
            let task = Uuid::parse_str(&task)
                .map_err(|_| integrity("the retained task reference is invalid"))?;
            tasks.push((task, intent.unwrap_or_else(|| "read_only".to_owned()), turn));
        }
        Ok(tasks)
    }

    /// The retained dispatch-index object of one task (Part A).
    pub(crate) fn dispatch_index_object(
        &self,
        task_id: Uuid,
    ) -> Result<Option<Vec<u8>>, MachineError> {
        crate::storage_observation::read_content(
            &self.connection,
            "dispatch_index",
            &task_id.to_string(),
            "index",
        )
    }

    /// The complete bytes retained for one task's dirty and untracked paths.
    pub(crate) fn dispatch_index_baseline(
        &self,
        task_id: Uuid,
    ) -> Result<Vec<(String, Vec<u8>)>, MachineError> {
        let owner = task_id.to_string();
        let mut statement = self
            .connection
            .prepare(
                "SELECT representation FROM result_content_refs
                  WHERE owner_kind='dispatch_index' AND owner_id=?1
                    AND representation LIKE 'baseline:%' ORDER BY representation",
            )
            .map_err(internal)?;
        let representations = statement
            .query_map([owner.as_str()], |row| row.get::<_, String>(0))
            .map_err(internal)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)?;
        let mut baseline = Vec::with_capacity(representations.len());
        for representation in representations {
            let path = representation
                .strip_prefix("baseline:")
                .ok_or_else(|| integrity("the retained baseline representation is invalid"))?
                .to_owned();
            let bytes = crate::storage_observation::read_content(
                &self.connection,
                "dispatch_index",
                &owner,
                &representation,
            )?
            .ok_or_else(|| integrity("the retained baseline bytes are missing"))?;
            baseline.push((path, bytes));
        }
        Ok(baseline)
    }

    /// How many write tasks of one member are in flight (cleanup fact).
    pub(crate) fn active_write_task_count(
        connection: &Connection,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
    ) -> Result<u64, MachineError> {
        connection
            .query_row(
                "SELECT COUNT(*) FROM tasks
                  WHERE engagement_id=?1 AND specialist_run_id=?2
                    AND state IN ('dispatching','running')
                    AND execution_intent='canonical_workspace_write'",
                params![engagement_id.to_string(), specialist_run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)
    }

    /// The successor task one member Turn is bound to.
    ///
    /// A worker settles the task whose dispatch recorded exactly this Turn id, so
    /// a later Turn on the same member never settles an earlier one.
    pub(crate) fn task_for_member_turn(
        &self,
        engagement_id: Uuid,
        specialist_run_id: Uuid,
        turn_id: &str,
    ) -> Result<Option<(Uuid, String)>, MachineError> {
        let row: Option<(String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT task_id,execution_intent FROM tasks
                  WHERE engagement_id=?1 AND specialist_run_id=?2 AND turn_id=?3
                    AND state IN ('dispatching','running')
                  ORDER BY created_at_ms DESC LIMIT 1",
                params![
                    engagement_id.to_string(),
                    specialist_run_id.to_string(),
                    turn_id
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((task, intent)) = row else {
            return Ok(None);
        };
        let task = Uuid::parse_str(&task)
            .map_err(|_| integrity("the retained task reference is invalid"))?;
        Ok(Some((
            task,
            intent.unwrap_or_else(|| "read_only".to_owned()),
        )))
    }

    /// The published complete result of one task.
    ///
    /// The bytes live in the content store the acceptance already uses; the task
    /// row only names the digest and the artifact.
    pub(crate) fn task_complete_result(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<Option<(String, Vec<u8>)>, MachineError> {
        let row: Option<(Option<String>, Option<String>)> = self
            .connection
            .query_row(
                "SELECT result_sha256,artifact_id FROM tasks
                  WHERE engagement_id=?1 AND task_id=?2",
                params![engagement_id.to_string(), task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((Some(digest), Some(artifact))) = row else {
            return Ok(None);
        };
        let artifact_id = Uuid::parse_str(&artifact)
            .map_err(|_| integrity("the retained artifact reference is invalid"))?;
        let bytes = crate::storage_observation::read_content(
            &self.connection,
            "external_artifact",
            &artifact_id.to_string(),
            "json",
        )?
        .ok_or_else(|| integrity("the retained complete result bytes are missing"))?;
        Ok(Some((digest, bytes)))
    }

    /// The accepted task identity one member task retained.
    pub(crate) fn external_task_identity(
        &self,
        engagement_id: Uuid,
        task_id: Uuid,
    ) -> Result<Option<crate::delegation_identity::AcceptedTask>, MachineError> {
        let request: Option<Option<String>> = self
            .connection
            .query_row(
                "SELECT request_json FROM tasks WHERE engagement_id=?1 AND task_id=?2",
                params![engagement_id.to_string(), task_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?;
        let Some(Some(request)) = request else {
            return Ok(None);
        };
        let accepted: crate::delegation_identity::AcceptedTask = serde_json::from_str(&request)
            .map_err(|_| integrity("the accepted task identity is invalid"))?;
        // The row must still be the identity the acceptance recorded, not a
        // rewrite of the row and its bytes together.
        let recorded: Option<(String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT objective_sha256,external_request_ref_json FROM tasks
                  WHERE engagement_id=?1 AND task_id=?2",
                params![engagement_id.to_string(), task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let (objective, external) =
            recorded.ok_or_else(|| integrity("the retained task is missing"))?;
        if let Some(external) = external {
            let external: serde_json::Value = serde_json::from_str(&external)
                .map_err(|_| integrity("the retained task reference is invalid"))?;
            if let Some(accepted_digest) = external
                .get("accepted_task_digest")
                .and_then(serde_json::Value::as_str)
            {
                let digest = accepted.digest()?;
                if accepted_digest != digest || objective != digest {
                    return Err(integrity(
                        "the retained task identity differs from the accepted one",
                    ));
                }
            }
        }
        // The retained bytes are the acceptance's own evidence: every reference
        // must still resolve to the byte it names.
        let (task_body, contexts, target) = accepted_task_content(&self.connection, task_id)?
            .ok_or_else(|| integrity("the accepted task bytes are missing"))?;
        let digest_of = |bytes: &[u8]| format!("sha256:{}", sha256_hex(bytes));
        if digest_of(&task_body) != accepted.task_body.sha256 {
            return Err(integrity(
                "the accepted task body differs from its reference",
            ));
        }
        for reference in &accepted.contexts {
            let bytes = match reference.kind.as_str() {
                "accepted_context" => contexts
                    .iter()
                    .find(|(identity, _)| identity == &reference.identity)
                    .map(|(_, bytes)| bytes),
                "review_target" => target.as_ref(),
                _ => None,
            }
            .ok_or_else(|| integrity("the accepted task context is missing"))?;
            if digest_of(bytes) != reference.content.sha256 {
                return Err(integrity(
                    "the accepted task context differs from its reference",
                ));
            }
        }
        Ok(Some(accepted))
    }

    /// Read one keyed intent namespace without any write effect.
    pub(crate) fn intent_lookup(
        &self,
        namespace: &LookupNamespace,
    ) -> Result<IntentLookup, MachineError> {
        // One deferred read transaction: the collision, key, refusal and
        // watermark reads observe one consistent snapshot, so an `Absent` proof
        // can never carry a watermark that already includes a matching row.
        let transaction = self.connection.unchecked_transaction().map_err(internal)?;
        intent_lookup(&transaction, namespace)
    }

    pub(crate) fn scope_membership_revision(
        &self,
        workspace_ref: &str,
        scope_kind: &str,
        scope_ref: &str,
    ) -> Result<u64, MachineError> {
        scope_membership_revision(&self.connection, workspace_ref, scope_kind, scope_ref)
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
                        "UPDATE tasks SET state=?3,safe_error_code=?4,diagnostic_json=?5 WHERE engagement_id=?1 AND task_id=?2",
                        params![
                            engagement_id.to_string(),
                            task_id.to_string(),
                            state,
                            completion.safe_error_code,
                            completion.diagnostic.map(serde_json::to_string).transpose().map_err(internal)?
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

/// Retain the exact accepted task bytes with the acceptance.
///
/// The content store keeps one representation per accepted reference, so a
/// later dispatch renders the Turn from these bytes instead of re-reading
/// caller input.
pub(crate) fn retain_accepted_task_content(
    transaction: &Transaction<'_>,
    task_id: Uuid,
    task_body: &[u8],
    contexts: &[(String, Vec<u8>)],
    target: Option<&[u8]>,
    _now_ms: i64,
) -> Result<(), MachineError> {
    crate::storage_observation::put_content(
        transaction,
        "accepted_task",
        &task_id.to_string(),
        "task_body",
        task_body,
    )?;
    for (identity, bytes) in contexts {
        crate::storage_observation::put_content(
            transaction,
            "accepted_task",
            &task_id.to_string(),
            &format!("context:{identity}"),
            bytes,
        )?;
    }
    if let Some(target) = target {
        crate::storage_observation::put_content(
            transaction,
            "accepted_task",
            &task_id.to_string(),
            "target",
            target,
        )?;
    }
    Ok(())
}

/// The retained accepted-task bytes of one dispatch: the canonical task body,
/// one body per accepted context in accepted order, and the optional direct
/// target.
pub(crate) type AcceptedTaskBytes = (Vec<u8>, Vec<(String, Vec<u8>)>, Option<Vec<u8>>);

/// Read the exact accepted task bytes back for one dispatch.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn accepted_task_content(
    connection: &Connection,
    task_id: Uuid,
) -> Result<Option<AcceptedTaskBytes>, MachineError> {
    let owner = task_id.to_string();
    let Some(task_body) =
        crate::storage_observation::read_content(connection, "accepted_task", &owner, "task_body")?
    else {
        return Ok(None);
    };
    let mut contexts = Vec::new();
    let mut statement = connection
        .prepare(
            "SELECT representation FROM result_content_refs
              WHERE owner_kind='accepted_task' AND owner_id=?1
                AND representation LIKE 'context:%' ORDER BY rowid",
        )
        .map_err(internal)?;
    let representations = statement
        .query_map([owner.as_str()], |row| row.get::<_, String>(0))
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    for representation in representations {
        let identity = representation
            .strip_prefix("context:")
            .ok_or_else(|| integrity("accepted context representation is invalid"))?
            .to_owned();
        let bytes = crate::storage_observation::read_content(
            connection,
            "accepted_task",
            &owner,
            &representation,
        )?
        .ok_or_else(|| integrity("accepted context bytes are missing"))?;
        contexts.push((identity, bytes));
    }
    let target =
        crate::storage_observation::read_content(connection, "accepted_task", &owner, "target")?;
    Ok(Some((task_body, contexts, target)))
}

/// Retain a validated canonical checked payload with its result transaction.
pub(crate) fn retain_checked_task_output(
    transaction: &Transaction<'_>,
    task: Uuid,
    bytes: &[u8],
    response: &[u8],
) -> Result<(), MachineError> {
    crate::storage_observation::put_content(
        transaction,
        "accepted_task",
        &task.to_string(),
        "structured_output",
        bytes,
    )?;
    crate::storage_observation::put_content(
        transaction,
        "accepted_task",
        &task.to_string(),
        "final_response",
        response,
    )
    .map(|_| ())
}

/// Retain one task's result bytes and their artifact row.
///
/// The bytes go to the same content store the legacy result path uses, and the
/// artifact row names their digest so every reader observes one object.
pub(crate) fn retain_task_result(
    transaction: &Transaction<'_>,
    engagement_id: Uuid,
    task_id: Uuid,
    bytes: &[u8],
    digest: &str,
) -> Result<Uuid, MachineError> {
    let artifact_id = Uuid::now_v7();
    crate::storage_observation::put_content(
        transaction,
        "external_artifact",
        &artifact_id.to_string(),
        "json",
        bytes,
    )?;
    let specialist: String = transaction
        .query_row(
            "SELECT specialist_run_id FROM tasks WHERE engagement_id=?1 AND task_id=?2",
            params![engagement_id.to_string(), task_id.to_string()],
            |row| row.get(0),
        )
        .map_err(internal)?;
    transaction
        .execute(
            "INSERT INTO artifacts(
               artifact_id,engagement_id,task_id,specialist_run_id,
               result_sha256,canonical_json
             ) VALUES(?1,?2,?3,?4,?5,'')",
            params![
                artifact_id.to_string(),
                engagement_id.to_string(),
                task_id.to_string(),
                specialist,
                digest
            ],
        )
        .map_err(internal)?;
    Ok(artifact_id)
}

/// One member task reservation, without its legacy receipt or commit.
pub(crate) struct ExternalTaskReservationCore<'a> {
    pub engagement_id: Uuid,
    pub specialist_run_id: Uuid,
    pub task_id: Uuid,
    /// `None` when the admitted task carries no separate objective digest.
    pub objective_sha256: Option<&'a str>,
    pub external_request_json: Option<&'a str>,
    pub request_json: &'a str,
    pub request_sha256: &'a str,
    pub execution_intent: &'a str,
    pub deadline_seconds: u64,
    pub now_ms: i64,
}

/// Reserve one member task inside the caller's transaction.
pub(crate) fn reserve_external_task_in(
    transaction: &Transaction<'_>,
    core: ExternalTaskReservationCore<'_>,
) -> Result<ExternalTaskSnapshot, MachineError> {
    require_current(
        transaction,
        core.engagement_id,
        &["active", "degraded", "recovering"],
    )?;
    let (member_state, requested_access): (String, Option<String>) = transaction
        .query_row(
            "SELECT state,requested_access FROM members
              WHERE engagement_id=?1 AND specialist_run_id=?2",
            params![
                core.engagement_id.to_string(),
                core.specialist_run_id.to_string()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(internal)?
        .ok_or_else(|| specialist_not_member(core.specialist_run_id))?;
    if member_state != "active" {
        return Err(conflict("Specialist membership is not active"));
    }
    if !access_allows(
        requested_access.as_deref().unwrap_or("read_only"),
        core.execution_intent,
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
            core.task_id.to_string(),
            core.engagement_id.to_string(),
            core.specialist_run_id.to_string(),
            core.objective_sha256,
            core.external_request_json,
            core.request_json,
            core.execution_intent,
            i64::try_from(core.deadline_seconds).map_err(internal)?,
            core.now_ms
        ],
    );
    if let Err(error) = inserted {
        let active: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks
                 WHERE specialist_run_id=?1
                   AND state IN ('accepted','queued','claimed','dispatching','running'))",
                [core.specialist_run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if active {
            return Err(conflict("Specialist already has an active task"));
        }
        return Err(internal(error));
    }
    let result = ExternalTaskSnapshot {
        task_id: core.task_id,
        specialist_run_id: core.specialist_run_id,
        state: "accepted".to_owned(),
        turn_id: None,
        result_artifact_ref: None,
        result: None,
        safe_error_code: None,
        diagnostic: None,
    };
    transaction
        .execute(
            "UPDATE engagements SET revision=revision+1,updated_at_ms=?2 WHERE engagement_id=?1",
            params![core.engagement_id.to_string(), core.now_ms],
        )
        .map_err(internal)?;
    append_event(
        transaction,
        core.engagement_id,
        "external_task_reserved",
        core.request_sha256,
    )?;
    Ok(result)
}

/// One terminal task fact, without its legacy receipt or commit.
pub(crate) struct TaskTerminal<'a> {
    pub engagement_id: Uuid,
    pub task_id: Uuid,
    pub state: &'a str,
    pub safe_error_code: Option<&'a str>,
    pub result_sha256: Option<&'a str>,
    pub artifact_id: Option<Uuid>,
    pub diagnostic_json: Option<&'a str>,
    pub now_ms: i64,
}

/// Record one terminal task fact inside the caller's transaction.
///
/// Returns `None` when the task was already terminal, so each caller keeps its
/// own replay reasoning while the effect stays shared.
pub(crate) fn terminalize_external_task_in(
    transaction: &Transaction<'_>,
    core: TaskTerminal<'_>,
) -> Result<Option<ExternalTaskSnapshot>, MachineError> {
    let state: Option<String> = transaction
        .query_row(
            "SELECT state FROM tasks WHERE engagement_id=?1 AND task_id=?2",
            params![core.engagement_id.to_string(), core.task_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?;
    let Some(state) = state else {
        return Ok(None);
    };
    if !matches!(
        state.as_str(),
        "accepted" | "queued" | "claimed" | "dispatching" | "running"
    ) {
        return Ok(None);
    }
    let changed = transaction
        .execute(
            "UPDATE tasks SET state=?3,result_sha256=?4,artifact_id=?5,safe_error_code=?6,
                    updated_at_ms=?7,diagnostic_json=?8
              WHERE engagement_id=?1 AND task_id=?2
                AND state IN ('accepted','queued','claimed','dispatching','running')",
            params![
                core.engagement_id.to_string(),
                core.task_id.to_string(),
                core.state,
                core.result_sha256,
                core.artifact_id.map(|value| value.to_string()),
                core.safe_error_code,
                core.now_ms,
                core.diagnostic_json
            ],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Ok(None);
    }
    append_event(
        transaction,
        core.engagement_id,
        "external_task_terminal",
        core.state,
    )?;
    external_task_in(transaction, core.engagement_id, core.task_id).map(Some)
}

/// Move one accepted task to `dispatching` without a legacy receipt.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn mark_external_task_dispatching_in(
    transaction: &Transaction<'_>,
    engagement_id: Uuid,
    task_id: Uuid,
    now_ms: i64,
) -> Result<ExternalTaskSnapshot, MachineError> {
    let changed = transaction
        .execute(
            "UPDATE tasks SET state='dispatching',updated_at_ms=?3
              WHERE engagement_id=?1 AND task_id=?2 AND state='accepted'",
            params![engagement_id.to_string(), task_id.to_string(), now_ms],
        )
        .map_err(internal)?;
    if changed != 1 {
        let current = external_task_in(transaction, engagement_id, task_id)?;
        if current.state == "dispatching" {
            return Ok(current);
        }
        return Err(conflict("task is not at the accepted dispatch boundary"));
    }
    append_event(
        transaction,
        engagement_id,
        "external_task_dispatching",
        &task_id.to_string(),
    )?;
    external_task_in(transaction, engagement_id, task_id)
}

/// Move one dispatching task to `running` with its accepted Turn.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn mark_external_task_running_in(
    transaction: &Transaction<'_>,
    engagement_id: Uuid,
    task_id: Uuid,
    turn_id: &str,
    now_ms: i64,
) -> Result<ExternalTaskSnapshot, MachineError> {
    let changed = transaction
        .execute(
            "UPDATE tasks SET state='running',turn_id=?3,updated_at_ms=?4
              WHERE engagement_id=?1 AND task_id=?2 AND state IN ('dispatching','accepted','queued')",
            params![
                engagement_id.to_string(),
                task_id.to_string(),
                turn_id,
                now_ms
            ],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Err(conflict("task is not at an admissible dispatch boundary"));
    }
    append_event(transaction, engagement_id, "external_task_running", turn_id)?;
    external_task_in(transaction, engagement_id, task_id)
}

/// Record one dispatch index with its content inside the caller's transaction.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn record_dispatch_index_with_content(
    transaction: &Transaction<'_>,
    record: DispatchIndexRecord<'_>,
    index: &[u8],
    baseline: &[(String, Vec<u8>)],
    now_ms: i64,
) -> Result<(), MachineError> {
    crate::storage_observation::put_content(
        transaction,
        "dispatch_index",
        &record.task_id.to_string(),
        "index",
        index,
    )?;
    for (path, bytes) in baseline {
        crate::storage_observation::put_content(
            transaction,
            "dispatch_index",
            &record.task_id.to_string(),
            &format!("baseline:{path}"),
            bytes,
        )?;
    }
    record_dispatch_index(transaction, record, now_ms)
}

/// The admitted External Specialist Engagement owner authority.
pub(crate) fn require_external_owner_binding(
    binding: &ControllerBinding,
) -> Result<(), MachineError> {
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
    Ok(())
}

/// One engagement open, without its legacy receipt or commit.
pub(crate) struct ExternalOpenCore<'a> {
    pub engagement_id: Uuid,
    pub workspace_id: &'a str,
    pub controller_sha256: &'a str,
    pub bootstrap_operation_id: Uuid,
    /// `None` when the trusted successor open carries no external provenance.
    pub external_json: Option<&'a str>,
    pub external_sha256: Option<&'a str>,
    pub label: Option<&'a str>,
    pub binding_json: &'a str,
    pub now_ms: i64,
}

/// Create one External Specialist Engagement inside the caller's transaction.
///
/// The successor keyed open and the legacy facade-v4 open share this effect, so
/// the row, its bootstrap event and its revision cannot drift between them.
pub(crate) fn open_external_engagement_in(
    transaction: &Transaction<'_>,
    core: ExternalOpenCore<'_>,
) -> Result<ExternalEngagementSnapshot, MachineError> {
    transaction
        .execute(
            "INSERT INTO engagements(
               engagement_id,workspace_id,controller_sha256,state,revision,
               bootstrap_operation_id,external_controller_ref_json,
               external_controller_ref_sha256,label,controller_binding_json,
               created_at_ms,updated_at_ms,authority_kind
             ) VALUES(?1,?2,?3,'active',1,?4,?5,?6,?7,?8,?9,?9,'external_v1')",
            params![
                core.engagement_id.to_string(),
                core.workspace_id,
                core.controller_sha256,
                core.bootstrap_operation_id.to_string(),
                core.external_json,
                core.external_sha256,
                core.label,
                core.binding_json,
                core.now_ms,
            ],
        )
        .map_err(internal)?;
    // A successor open has no external provenance; the bootstrap payload still
    // records the absent provenance deterministically rather than guessing one.
    let external_sha256 = core
        .external_sha256
        .unwrap_or(NO_EXTERNAL_PROVENANCE_SHA256);
    let bootstrap_payload = format!(
        "{}\0{external_sha256}\0{}",
        core.bootstrap_operation_id, core.controller_sha256
    );
    append_event(
        transaction,
        core.engagement_id,
        "external_engagement_opened",
        &bootstrap_payload,
    )?;
    Ok(ExternalEngagementSnapshot {
        engagement_id: core.engagement_id,
        state: "active".to_owned(),
        external_controller_ref: core
            .external_json
            .map(|json| serde_json::from_str(json).map_err(internal))
            .transpose()?
            .unwrap_or(Value::Null),
        revision: 1,
        specialists: Vec::new(),
    })
}

/// One member reservation, without its legacy receipt or commit.
pub(crate) struct ExternalHireReservationCore<'a> {
    pub engagement_id: Uuid,
    pub hire_operation_id: Uuid,
    pub specialist_run_id: Uuid,
    pub role_ref: &'a str,
    pub configuration_json: &'a str,
    pub configuration_sha256: &'a str,
    /// `None` when the admitted member carries no separate objective.
    pub objective_sha256: Option<&'a str>,
    pub requested_access: &'a str,
    pub event_payload: &'a str,
    pub now_ms: i64,
}

/// Reserve one admitted member inside the caller's transaction.
pub(crate) fn reserve_external_hire_in(
    transaction: &Transaction<'_>,
    core: ExternalHireReservationCore<'_>,
) -> Result<ExternalHireReservation, MachineError> {
    require_current(
        transaction,
        core.engagement_id,
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
                core.specialist_run_id.to_string(),
                core.engagement_id.to_string(),
                core.hire_operation_id.to_string(),
                core.configuration_sha256,
                core.role_ref,
                core.configuration_json,
                core.objective_sha256,
                core.requested_access,
                core.now_ms
            ],
        )
        .map_err(internal)?;
    transaction
        .execute(
            "UPDATE engagements SET revision=revision+1,updated_at_ms=?2 WHERE engagement_id=?1",
            params![core.engagement_id.to_string(), core.now_ms],
        )
        .map_err(internal)?;
    append_event(
        transaction,
        core.engagement_id,
        "external_hire_reserved",
        core.event_payload,
    )?;
    Ok(ExternalHireReservation {
        engagement_id: core.engagement_id,
        hire_operation_id: core.hire_operation_id,
        specialist_run_id: core.specialist_run_id,
        state: "provisioning".to_owned(),
    })
}

/// Settle one reserved member from its Run start outcome.
///
/// Returns `None` when the member was already settled, so each caller keeps its
/// own replay reasoning and the effect stays shared.
pub(crate) fn settle_external_hire_in(
    transaction: &Transaction<'_>,
    reservation: &ExternalHireReservation,
    outcome: RuntimeOutcome,
    now_ms: i64,
) -> Result<Option<ExternalHireReservation>, MachineError> {
    let state = match outcome {
        RuntimeOutcome::Accepted => "active",
        RuntimeOutcome::Rejected | RuntimeOutcome::Unknown => "degraded",
    };
    let response_state = match outcome {
        RuntimeOutcome::Accepted => "ready",
        RuntimeOutcome::Rejected | RuntimeOutcome::Unknown => "recovery_required",
    };
    let changed = transaction
        .execute(
            "UPDATE members SET state=?2,actor_residency=?3,updated_at_ms=?4
             WHERE specialist_run_id=?1 AND state='provisioning'",
            params![
                reservation.specialist_run_id.to_string(),
                state,
                match outcome {
                    RuntimeOutcome::Accepted => "unstarted",
                    RuntimeOutcome::Unknown => "recovering",
                    RuntimeOutcome::Rejected => "unavailable",
                },
                now_ms
            ],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Ok(None);
    }
    if outcome != RuntimeOutcome::Accepted {
        transaction
            .execute(
                "UPDATE engagements SET state='degraded',revision=revision+1,updated_at_ms=?2 WHERE engagement_id=?1",
                params![reservation.engagement_id.to_string(), now_ms],
            )
            .map_err(internal)?;
    }
    append_event(
        transaction,
        reservation.engagement_id,
        "external_hire_outcome",
        response_state,
    )?;
    Ok(Some(ExternalHireReservation {
        state: response_state.to_owned(),
        ..reservation.clone()
    }))
}

/// The verified generation state of an existing Engagement database.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmittedGeneration {
    /// Base generation 1 through 5; the next writable open migrates in place.
    Legacy(i64),
    /// Base generation 6, the current accepted shape.
    Six,
}

/// One effect-free observation of an existing Engagement database.
///
/// The probe never creates a file, a lock, a WAL sidecar or a directory, never
/// changes a permission and never runs DDL. It refuses to guess: a database
/// whose generation keys are absent, unreadable or outside the supported set is
/// a typed unsupported-generation or recovery failure, not a legacy shape.
pub(crate) struct GenerationProbe {
    schema_generation: AdmittedGeneration,
    namespace_birth_id: Option<String>,
}

/// How one probe observes an existing database without writing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProbeOpen {
    /// No sidecar exists: an immutable read can neither create nor write one.
    Immutable,
    /// A live WAL pair or a rollback journal: an ordinary read-only open
    /// observes coordination state and rolls back a hot journal.
    ReadOnly,
    /// A one-sided WAL pair that persisted across re-observation.
    RecoveryRequired,
}

/// How many times a transient one-sided sidecar pair is re-observed before it is
/// classified as recovery, and how long each re-observation waits.
const SIDECAR_REOBSERVATIONS: usize = 5;
const SIDECAR_REOBSERVE_DELAY: Duration = Duration::from_millis(10);

/// The open mode for one observed sidecar state.
const fn probe_open_mode(state: SidecarState) -> ProbeOpen {
    match state {
        SidecarState::Neither => ProbeOpen::Immutable,
        SidecarState::Both | SidecarState::JournalOnly => ProbeOpen::ReadOnly,
        SidecarState::LogOnly | SidecarState::MemoryOnly => ProbeOpen::RecoveryRequired,
    }
}

/// Settle a one-sided sidecar observation.
///
/// SQLite is not atomic about the pair: a WAL open creates `-wal` before `-shm`,
/// and the last close unlinks `-shm` before deleting `-wal`. A concurrent probe
/// can therefore see a one-sided pair during a normal open or close, which is not
/// a recovery condition. A one-sided state is re-observed a bounded number of
/// times and only a persistent one is classified as recovery.
fn settle_sidecars(
    path: &Path,
    observe: &mut impl FnMut(&Path) -> Result<SidecarState, MachineError>,
) -> Result<SidecarState, MachineError> {
    let mut state = observe(path)?;
    for _ in 0..SIDECAR_REOBSERVATIONS {
        if probe_open_mode(state) != ProbeOpen::RecoveryRequired {
            return Ok(state);
        }
        std::thread::sleep(SIDECAR_REOBSERVE_DELAY);
        state = observe(path)?;
    }
    Ok(state)
}

impl GenerationProbe {
    fn open(path: &Path) -> Result<Option<Self>, MachineError> {
        let sidecars = settle_sidecars(path, &mut SidecarState::observe)?;
        let mode = probe_open_mode(sidecars);
        if mode == ProbeOpen::RecoveryRequired {
            return Err(recovery_required(
                if sidecars == SidecarState::LogOnly {
                    "wal"
                } else {
                    "shm"
                },
                if sidecars == SidecarState::LogOnly {
                    "log without memory"
                } else {
                    "memory without log"
                },
            ));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let connection = if mode == ProbeOpen::Immutable {
            // `immutable=1` is the only open that cannot write, but it also skips
            // hot-journal rollback, so it is used only when no sidecar at all
            // exists.
            Connection::open_with_flags(
                crate::workspace::immutable_database_uri(path)?,
                flags | OpenFlags::SQLITE_OPEN_URI,
            )
        } else {
            Connection::open_with_flags(path, flags)
        }
        .map_err(|_| database_unavailable())?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(internal)?;
        // A database with no schema objects was created but never bootstrapped:
        // `Connection::open` creates the file before the bootstrap commits, so a
        // crash there must stay reachable. Creating openers bootstrap it; readers
        // treat it as missing.
        let objects: i64 = connection
            .query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get(0))
            .map_err(|_| database_unavailable())?;
        if objects == 0 {
            return Ok(None);
        }
        let has_metadata: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='metadata')",
                [],
                |row| row.get(0),
            )
            .map_err(|_| database_unavailable())?;
        if !has_metadata {
            // Generation 1 created `metadata` at birth, so a non-empty database
            // without it is not a verified legacy shape.
            return Err(MachineError::new(
                "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                "orchestration database does not carry a verified schema generation",
                false,
                serde_json::json!({"observed":null}),
            ));
        }
        let read = |key: &str| -> Result<Option<String>, MachineError> {
            connection
                .query_row("SELECT value FROM metadata WHERE key=?1", [key], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(|_| database_unavailable())
        };
        let observed = read("schema_version")?;
        let schema_generation = match observed.as_deref() {
            Some("1" | "2" | "3" | "4" | "5") => AdmittedGeneration::Legacy(
                observed
                    .as_deref()
                    .unwrap()
                    .parse::<i64>()
                    .map_err(|_| database_unavailable())?,
            ),
            Some("6") => AdmittedGeneration::Six,
            _ => {
                return Err(MachineError::new(
                    "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                    "orchestration schema is not supported",
                    false,
                    serde_json::json!({"observed":observed}),
                ));
            }
        };
        // Validate the extension generation here; it is not retained.
        match read("orchestration_schema_version")? {
            // External-only workspaces never create the extension tables.
            None => {}
            Some(value) if matches!(value.as_str(), "1" | "2" | "3") => {}
            Some(value) => {
                return Err(MachineError::new(
                    "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                    "orchestration extension schema is not supported",
                    false,
                    serde_json::json!({"observed":value}),
                ));
            }
        }
        if matches!(schema_generation, AdmittedGeneration::Legacy(_))
            && !connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' \
                     AND name='engagements')",
                    [],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(|_| database_unavailable())?
        {
            return Err(MachineError::new(
                "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                "orchestration database does not carry a verified legacy shape",
                false,
                serde_json::json!({"observed":observed}),
            ));
        }
        let namespace_birth_id = read("namespace_birth_id")?;
        drop(connection);
        Ok(Some(Self {
            schema_generation,
            namespace_birth_id,
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SidecarState {
    /// No log, shared memory or rollback journal.
    Neither,
    /// A live WAL pair, which is recoverable coordination state.
    Both,
    /// A rollback journal, which a plain read-only open either rolls back or
    /// fails closed on.
    JournalOnly,
    LogOnly,
    MemoryOnly,
}

impl SidecarState {
    fn observe(path: &Path) -> Result<Self, MachineError> {
        let log = sidecar(path, "-wal")?;
        let memory = sidecar(path, "-shm")?;
        let journal = sidecar(path, "-journal")?;
        Ok(match (log, memory, journal) {
            (false, false, false) => Self::Neither,
            (false, false, true) => Self::JournalOnly,
            (true, true, _) => Self::Both,
            (true, false, _) => Self::LogOnly,
            (false, true, _) => Self::MemoryOnly,
        })
    }
}

fn sidecar(path: &Path, suffix: &str) -> Result<bool, MachineError> {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    match std::fs::symlink_metadata(Path::new(&name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(database_unavailable()),
    }
}

/// The effect-free pre-effect generation gate shared by every opener.
///
/// A missing database is reported as `None` so a creating caller can bootstrap
/// it; an existing database yields its verified generation. Nothing here has a
/// filesystem or database write effect.
pub(crate) fn admit_generation(path: &Path) -> Result<Option<GenerationProbe>, MachineError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(database_unavailable()),
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(database_unavailable());
        }
        Ok(_) => {}
    }
    GenerationProbe::open(path)
}

fn database_unavailable() -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "the orchestration database is unavailable",
        false,
        serde_json::json!({"invariant":"orchestration database observation failed"}),
    )
}

fn recovery_required(subject: &str, state: &str) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "the orchestration database sidecar state is inconsistent",
        false,
        serde_json::json!({"subject":subject,"state":state,"required_action":"restore_state"}),
    )
}

/// The set-up state of the workspace delegation namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelegationNamespace {
    /// Neither the database nor the birth record exists.
    NotSetUp,
    /// A generation-6 database whose birth record matches.
    Ready { birth_id: Uuid },
    /// A birth record and a database disagree about the namespace.
    RecoveryRequired,
}

impl DelegationNamespace {
    #[must_use]
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// Where the namespace birth record lives for one workspace state root.
#[must_use]
pub fn namespace_birth_path(state_root: &Path) -> PathBuf {
    state_root.join("orchestration").join(NAMESPACE_BIRTH_FILE)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceBirth {
    schema: String,
    namespace_birth_id: String,
}

/// Read-only namespace authority check for callers that must not create state.
///
/// Successor keyed operations and lookups use this before any store open: a
/// missing namespace is `NotSetUp` (never authoritative absence), and an
/// inconsistent one is `RecoveryRequired`.
pub fn delegation_namespace(state_root: &Path) -> Result<DelegationNamespace, MachineError> {
    let path = EngagementStore::workspace_database_path(state_root);
    let file = read_birth_record(&birth_record_path(&path)?)?;
    let Some(probe) = admit_generation(&path)? else {
        return Ok(match file {
            Some(_) => DelegationNamespace::RecoveryRequired,
            None => DelegationNamespace::NotSetUp,
        });
    };
    if probe.schema_generation != AdmittedGeneration::Six {
        // A legacy database can never be migrated while a birth record already
        // names a namespace: that is a recovery condition, not a fresh setup.
        return Ok(match file {
            Some(_) => DelegationNamespace::RecoveryRequired,
            None => DelegationNamespace::NotSetUp,
        });
    }
    let Some(database_birth_id) = probe.namespace_birth_id else {
        return Ok(DelegationNamespace::RecoveryRequired);
    };
    let Ok(database_birth_id) = Uuid::parse_str(&database_birth_id) else {
        return Ok(DelegationNamespace::RecoveryRequired);
    };
    Ok(match file {
        Some(record) if record == database_birth_id => DelegationNamespace::Ready {
            birth_id: database_birth_id,
        },
        Some(_) => DelegationNamespace::RecoveryRequired,
        // The database is authoritative: a missing file is republished by the
        // next writable open.
        None => DelegationNamespace::Ready {
            birth_id: database_birth_id,
        },
    })
}

/// Require a set-up namespace, mapping its absence to the typed precondition.
pub fn require_delegation_namespace(state_root: &Path) -> Result<Uuid, MachineError> {
    match delegation_namespace(state_root)? {
        DelegationNamespace::Ready { birth_id } => Ok(birth_id),
        DelegationNamespace::NotSetUp => Err(MachineError::new(
            "DELEGATION_NAMESPACE_UNSET",
            "the workspace delegation namespace is not set up",
            false,
            serde_json::json!({"required_action":"restore_state","effect":"none"}),
        )),
        DelegationNamespace::RecoveryRequired => Err(MachineError::new(
            "RECOVERY_REQUIRED",
            "the workspace delegation namespace is inconsistent",
            false,
            serde_json::json!({"required_action":"restore_state","effect":"none"}),
        )),
    }
}

/// Explicit setup for a workspace that has no delegation namespace yet.
pub fn ensure_delegation_namespace(state_root: &Path) -> Result<Uuid, MachineError> {
    let path = EngagementStore::workspace_database_path(state_root);
    drop(EngagementStore::open(&path)?);
    require_delegation_namespace(state_root)
}

// The recording caller is the settlement port's dispatch path, which the
// successor adapters construct at run time; the unit tests exercise it.
/// One retained dispatch index for a canonical-write task.
///
/// The index object and its baseline bytes live in the content store; this row
/// owns only the digest, the bounds and the recorded `HEAD` anchor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DispatchIndexRecord<'a> {
    pub task_id: Uuid,
    pub workspace_ref: &'a str,
    pub run_ref: Uuid,
    pub head_commit: Option<&'a str>,
    /// Bare lowercase hex of the retained index object.
    pub index_sha256: &'a str,
    pub index_byte_length: u64,
    pub file_count: u64,
    pub baseline_bytes: u64,
}

/// Retain one dispatch index inside the accepted-task transaction.
///
/// Bounds: 10,000 files, a 64 MiB index-only ceiling, an 8 MiB-per-side
/// baseline already enforced by capture, and a 256 MiB aggregate over a Run's
/// retained baselines. Exceeding any bound is a typed capacity refusal, never a
/// truncated index.
pub(crate) fn record_dispatch_index(
    transaction: &Transaction<'_>,
    record: DispatchIndexRecord<'_>,
    now_ms: i64,
) -> Result<(), MachineError> {
    let bounded_hex = record.index_sha256.len() == 64
        && record
            .index_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !bounded_hex
        || record.index_byte_length == 0
        || record.index_byte_length > 67_108_864
        || record.file_count > 10_000
        || record.baseline_bytes > MAX_RUN_BASELINE_BYTES
    {
        return Err(MachineError::new(
            "STORAGE_CAPACITY_EXCEEDED",
            "the dispatch index exceeds its retained bounds",
            false,
            serde_json::json!({"subject":"dispatch_index","required_action":"fix_request"}),
        ));
    }
    let retained: u64 = run_retained_bytes(transaction, record.run_ref)?;
    if retained.saturating_add(record.baseline_bytes) > MAX_RUN_BASELINE_BYTES {
        return Err(MachineError::new(
            "STORAGE_CAPACITY_EXCEEDED",
            "the Run's retained dispatch baselines exceed the per-Run bound",
            false,
            serde_json::json!({"subject":"dispatch_index","required_action":"fix_request"}),
        ));
    }
    transaction
        .execute(
            "INSERT INTO dispatch_indexes(
               task_id,workspace_ref,run_ref,head_commit,index_sha256,index_byte_length,
               file_count,baseline_bytes,created_at_ms
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                record.task_id.to_string(),
                record.workspace_ref,
                record.run_ref.to_string(),
                record.head_commit,
                record.index_sha256,
                record.index_byte_length,
                record.file_count,
                record.baseline_bytes,
                now_ms
            ],
        )
        .map_err(|error| {
            use rusqlite::ErrorCode;
            if matches!(
                error,
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: ErrorCode::ConstraintViolation,
                        extended_code: rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
                        ..
                    },
                    _
                )
            ) {
                conflict("a dispatch index already exists for this task")
            } else {
                internal(error)
            }
        })?;
    Ok(())
}

// The successor producer reads the index object through `dispatch_index_object`,
// so no production caller reads this typed row yet; the unit tests exercise it.
/// The retained dispatch index of one task, if any.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn dispatch_index(
    connection: &Connection,
    task_id: Uuid,
) -> Result<Option<(String, u64, u64, u64)>, MachineError> {
    connection
        .query_row(
            "SELECT index_sha256,index_byte_length,file_count,baseline_bytes
               FROM dispatch_indexes WHERE task_id=?1",
            [task_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(internal)
}

/// One member's successor disposition counts.
///
/// `interrupted_unknown`/`unknown` tasks and counted-unresolved assignments are
/// `unresolved`, so they block `idle` and release terminal. The legacy
/// `member_task_state` keeps its own semantics and callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MemberQuiescence {
    /// `members.state == 'active'`: a released, degraded or still-provisioning
    /// member is never idle (MDC:626).
    pub member_active: bool,
    pub running: u64,
    pub queued: u64,
    pub claimed: u64,
    pub dispatching: u64,
    pub unresolved: u64,
    pub pending_interaction: bool,
    pub cleanup_pending: bool,
    pub membership_revision: u64,
    pub workload_revision: u64,
}

impl MemberQuiescence {
    /// Whether this member holds no work this store can see.
    #[must_use]
    pub(crate) fn idle(&self) -> bool {
        self.member_active
            && self.running == 0
            && self.queued == 0
            && self.claimed == 0
            && self.dispatching == 0
            && self.unresolved == 0
            && !self.pending_interaction
            && !self.cleanup_pending
    }
}

/// The successor quiescence of one member.
///
/// This store owns no interaction rows, so `pending_interaction` is always
/// `false` here — the read-side limit until TASK-079 reflects actual
/// interactions — and `cleanup_pending` is filled by the callers from their
/// writer observation, not by this query. The release gate is the reader that
/// consults the Run's own durable interactions separately. The counts below
/// are the execution facts a disposition is allowed to use.
pub(crate) fn successor_member_quiescence(
    connection: &Connection,
    workspace_ref: &str,
    scope_kind: &str,
    scope_ref: &str,
    member_ref: &str,
) -> Result<MemberQuiescence, MachineError> {
    let count = |state: &str| -> Result<u64, MachineError> {
        connection
            .query_row(
                "SELECT COUNT(*) FROM tasks
                  WHERE engagement_id=?1 AND specialist_run_id=?2 AND state=?3",
                params![scope_ref, member_ref, state],
                |row| row.get(0),
            )
            .map_err(internal)
    };
    let unresolved_tasks: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM tasks
              WHERE engagement_id=?1 AND specialist_run_id=?2
                AND state IN ('interrupted_unknown','unknown')",
            params![scope_ref, member_ref],
            |row| row.get(0),
        )
        .map_err(internal)?;
    // A counted-unresolved assignment is an uncertain dispatch: only the
    // accounting row proves it, so it is counted even when the task row settled.
    let uncertain: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM caller_scope_assignments a
               JOIN tasks t ON t.task_id = a.assignment_id
              WHERE t.engagement_id=?1 AND t.specialist_run_id=?2
                AND a.accounting_state='counted_unresolved'",
            params![scope_ref, member_ref],
            |row| row.get(0),
        )
        .map_err(internal)?;
    let member_state: Option<String> = connection
        .query_row(
            "SELECT state FROM members WHERE engagement_id=?1 AND specialist_run_id=?2",
            params![scope_ref, member_ref],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?;
    let member_active = member_state.as_deref() == Some("active");
    let membership_revision =
        scope_membership_revision(connection, workspace_ref, scope_kind, scope_ref)?;
    let workload_revision =
        member_workload_revision(connection, workspace_ref, scope_kind, scope_ref, member_ref)?;
    Ok(MemberQuiescence {
        member_active,
        running: count("running")?,
        queued: count("queued")?.saturating_add(count("accepted")?),
        claimed: count("claimed")?,
        dispatching: count("dispatching")?,
        unresolved: unresolved_tasks.saturating_add(uncertain),
        pending_interaction: false,
        cleanup_pending: false,
        membership_revision,
        workload_revision,
    })
}

/// One successor keyed admission, written `prepared` before its acceptance
/// transaction (MDC:60-61).
pub(crate) struct IntentAdmission<'a> {
    pub operation_ref: Uuid,
    pub namespace: &'a LookupNamespace,
    pub mutation_envelope_json: &'a str,
    pub mutation_envelope_digest: &'a str,
    pub expected_revision: Option<u64>,
    pub now_ms: i64,
}

/// What one `prepared` insert established for its key. The retained
/// receipt is boxed: it dominates the unit and refusal variants.
#[derive(Debug)]
pub(crate) enum IntentPreparation {
    /// This call owns the key and wrote the `prepared` intent.
    Created,
    /// The key already owns this exact accepted intent: replay its receipt.
    Accepted(Box<OperationReceipt>),
    /// The key owns an intent whose effect is not settled; never redispatch it.
    Unresolved { operation_ref: Uuid },
}

/// One complete lookup result before it is projected onto the closed shape.
#[derive(Debug)]
pub(crate) enum IntentLookup {
    Accepted(Box<OperationReceipt>),
    NotAccepted {
        operation_ref: Uuid,
        input_digest: String,
        refusal_code: String,
        durable_refusal_revision: u64,
    },
    Unresolved {
        operation_ref: Uuid,
        known_subjects: Box<AcceptedSubjects>,
    },
    /// Only a complete read of a set-up namespace can establish this.
    Absent {
        durable_watermark: u64,
    },
}

/// The five caller references that name one successor intent key.
type IntentKey<'a> = (
    &'a str,
    &'a str,
    i64,
    Option<&'a str>,
    Option<&'a str>,
    &'a str,
    &'a str,
);

fn intent_key(namespace: &LookupNamespace) -> IntentKey<'_> {
    (
        &namespace.workspace_ref,
        &namespace.controller_principal,
        i64::try_from(namespace.controller_generation).unwrap_or(i64::MAX),
        namespace.scope_kind.as_deref(),
        namespace.scope_ref.as_deref(),
        &namespace.operation_kind,
        &namespace.idempotency_key,
    )
}

const INTENT_ROW: &str = "SELECT operation_ref,state,mutation_envelope_digest,receipt_json,
        accepted_subjects_json,mutation_envelope_json
   FROM delegation_operations
  WHERE workspace_ref=?1 AND controller_principal=?2 AND controller_generation=?3
    AND scope_kind IS ?4 AND scope_ref IS ?5 AND operation_kind=?6 AND idempotency_key=?7
    AND state <> 'resolved_not_accepted'";

/// Write one `prepared` intent, or report what the key already owns.
///
/// The legacy-key collision check runs inside this transaction, so a caller
/// that admits directly cannot create a second owner for a legacy key.
pub(crate) fn intent_prepare(
    transaction: &Transaction<'_>,
    admission: IntentAdmission<'_>,
) -> Result<IntentPreparation, MachineError> {
    let namespace = admission.namespace;
    checked(&namespace.idempotency_key, 256, "idempotency_key")?;
    checked(&namespace.operation_kind, 128, "operation_kind")?;
    checked(&namespace.workspace_ref, 512, "workspace_ref")?;
    checked(&namespace.controller_principal, 512, "controller_principal")?;
    if let Some(scope) = &namespace.scope_ref {
        checked(scope, 512, "scope_ref")?;
    }
    legacy_operation_collision(transaction, namespace)?;
    let key = intent_key(namespace);
    let existing: Option<(String, String, String, Option<String>, String, String)> = transaction
        .query_row(
            INTENT_ROW,
            params![key.0, key.1, key.2, key.3, key.4, key.5, key.6],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(internal)?;
    if let Some((operation_ref, state, digest, receipt, subjects, envelope)) = existing {
        if digest != admission.mutation_envelope_digest {
            return Err(intent_conflict());
        }
        let operation_ref = intent_operation_ref(&operation_ref)?;
        return match state.as_str() {
            "accepted" => Ok(IntentPreparation::Accepted(Box::new(intent_receipt(
                namespace,
                operation_ref,
                &digest,
                &envelope,
                receipt.as_deref(),
                &subjects,
            )?))),
            "prepared" | "unresolved" => {
                intent_envelope(namespace, &digest, &envelope)?;
                Ok(IntentPreparation::Unresolved { operation_ref })
            }
            _ => Err(intent_corrupt("the retained intent state is unknown")),
        };
    }
    // A definitively refused key may be retried only with the latest
    // refused accepted-input digest; any other identity is a conflict.
    if let Some(refused) = latest_refusal_digest(transaction, namespace)?
        && refused != admission.mutation_envelope_digest
    {
        return Err(intent_conflict());
    }
    let subjects = canonical_string(
        &serde_json::to_value(AcceptedSubjects::default())
            .map_err(|_| intent_corrupt("the prepared subjects are unrepresentable"))?,
    )?;
    transaction
        .execute(
            "INSERT INTO delegation_operations(
               operation_ref,state,workspace_ref,controller_principal,controller_generation,
               scope_kind,scope_ref,operation_kind,idempotency_key,mutation_envelope_json,
               mutation_envelope_digest,expected_revision,trusted_invocation_json,
               accepted_subjects_json,created_at_ms,updated_at_ms
             ) VALUES(?1,'prepared',?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL,?12,?13,?13)",
            params![
                admission.operation_ref.to_string(),
                key.0,
                key.1,
                key.2,
                key.3,
                key.4,
                key.5,
                key.6,
                admission.mutation_envelope_json,
                admission.mutation_envelope_digest,
                admission.expected_revision.map(|revision| revision as i64),
                subjects,
                admission.now_ms
            ],
        )
        .map_err(|error| {
            if intent_constraint(&error) {
                intent_conflict()
            } else {
                internal(error)
            }
        })?;
    Ok(IntentPreparation::Created)
}

/// Accept one prepared intent: the receipt and the effect commit together.
///
/// The retained receipt is immutable after this transition, and the
/// legacy-key collision check runs again inside the acceptance transaction.
pub(crate) fn intent_accept(
    transaction: &Transaction<'_>,
    operation_ref: Uuid,
    receipt: &OperationReceipt,
    now_ms: i64,
) -> Result<(), MachineError> {
    receipt
        .validate()
        .map_err(|_| intent_corrupt("the accepted receipt is invalid"))?;
    let namespace = &receipt.namespace;
    if receipt.operation_ref != operation_ref.to_string() {
        return Err(intent_corrupt("the receipt names another operation"));
    }
    legacy_operation_collision(transaction, namespace)?;
    let key = intent_key(namespace);
    let retained: Option<(String, String)> = transaction
        .query_row(
            "SELECT state,mutation_envelope_digest FROM delegation_operations
              WHERE operation_ref=?1 AND workspace_ref=?2 AND controller_principal=?3
                AND controller_generation=?4 AND scope_kind IS ?5 AND scope_ref IS ?6
                AND operation_kind=?7 AND idempotency_key=?8",
            params![
                operation_ref.to_string(),
                key.0,
                key.1,
                key.2,
                key.3,
                key.4,
                key.5,
                key.6
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((state, digest)) = retained else {
        return Err(intent_corrupt("the accepted intent is missing"));
    };
    if digest != receipt.accepted_input_digest {
        return Err(intent_corrupt(
            "the receipt carries another accepted input digest",
        ));
    }
    if !matches!(state.as_str(), "prepared" | "unresolved") {
        return Err(intent_conflict());
    }
    let subjects = canonical_string(
        &serde_json::to_value(&receipt.accepted_subjects)
            .map_err(|_| intent_corrupt("the accepted subjects are unrepresentable"))?,
    )?;
    let body = canonical_string(
        &serde_json::to_value(receipt)
            .map_err(|_| intent_corrupt("the receipt is unrepresentable"))?,
    )?;
    let changed = transaction
        .execute(
            "UPDATE delegation_operations
                SET state='accepted',receipt_json=?2,accepted_subjects_json=?3,updated_at_ms=?4
              WHERE operation_ref=?1 AND state IN ('prepared','unresolved')",
            params![operation_ref.to_string(), body, subjects, now_ms],
        )
        .map_err(|error| {
            if intent_constraint(&error) {
                intent_corrupt("the retained intent refused its acceptance transition")
            } else {
                internal(error)
            }
        })?;
    if changed != 1 {
        return Err(intent_conflict());
    }
    Ok(())
}

/// Resolve one intent as definitively not accepted before any effect (Case A).
///
/// The refusal revision is the refused row's durable sequence, which is what
/// `lookup_not_accepted` reports as `durable_refusal_revision`.
pub(crate) fn intent_refuse_before_acceptance(
    transaction: &Transaction<'_>,
    operation_ref: Uuid,
    refusal_code: &str,
    now_ms: i64,
) -> Result<u64, MachineError> {
    if refusal_code.is_empty()
        || refusal_code.len() > 128
        || !refusal_code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(invalid("refusal_code", "refusal code is not a safe code"));
    }
    let sequence: i64 = transaction
        .query_row(
            "SELECT sequence FROM delegation_operations WHERE operation_ref=?1",
            [operation_ref.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?
        .ok_or_else(|| intent_corrupt("the refused intent is missing"))?;
    let changed = transaction
        .execute(
            "UPDATE delegation_operations
                SET state='resolved_not_accepted',definitive_refusal=?2,
                    refusal_revision=?3,updated_at_ms=?4
              WHERE operation_ref=?1 AND state IN ('prepared','unresolved')",
            params![operation_ref.to_string(), refusal_code, sequence, now_ms],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Err(intent_conflict());
    }
    u64::try_from(sequence).map_err(|_| intent_corrupt("the refusal revision is invalid"))
}

/// Mark one intent's effect unresolved; its accepted receipt never changes.
pub(crate) fn intent_mark_unresolved(
    transaction: &Transaction<'_>,
    operation_ref: Uuid,
    now_ms: i64,
) -> Result<(), MachineError> {
    let changed = transaction
        .execute(
            "UPDATE delegation_operations SET state='unresolved',updated_at_ms=?2
              WHERE operation_ref=?1 AND state='prepared'",
            params![operation_ref.to_string(), now_ms],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Err(intent_conflict());
    }
    Ok(())
}

/// Read one keyed intent namespace under its four closed lookup states.
///
/// The read is effect free: it never creates the database, never writes, and it
/// refuses a legacy-owned key or a corrupt retained row instead of reporting
/// authoritative absence (MDC:186-200).
pub(crate) fn intent_lookup(
    connection: &Connection,
    namespace: &LookupNamespace,
) -> Result<IntentLookup, MachineError> {
    let key = intent_key(namespace);
    let existing: Option<(String, String, String, Option<String>, String, String)> = connection
        .query_row(
            INTENT_ROW,
            params![key.0, key.1, key.2, key.3, key.4, key.5, key.6],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(internal)?;
    if let Some((operation_ref, state, digest, receipt, subjects, envelope)) = existing {
        let operation_ref = intent_operation_ref(&operation_ref)?;
        return match state.as_str() {
            "accepted" => Ok(IntentLookup::Accepted(Box::new(intent_receipt(
                namespace,
                operation_ref,
                &digest,
                &envelope,
                receipt.as_deref(),
                &subjects,
            )?))),
            "prepared" | "unresolved" => {
                intent_envelope(namespace, &digest, &envelope)?;
                Ok(IntentLookup::Unresolved {
                    operation_ref,
                    known_subjects: Box::new(intent_subjects(&subjects)?),
                })
            }
            _ => Err(intent_corrupt("the retained intent state is unknown")),
        };
    }
    legacy_operation_collision(connection, namespace)?;
    let refusal: Option<(String, String, String, i64, String)> = connection
        .query_row(
            "SELECT operation_ref,mutation_envelope_digest,definitive_refusal,refusal_revision,
                    mutation_envelope_json
               FROM delegation_operations
              WHERE workspace_ref=?1 AND controller_principal=?2 AND controller_generation=?3
                AND scope_kind IS ?4 AND scope_ref IS ?5 AND operation_kind=?6
                AND idempotency_key=?7 AND state='resolved_not_accepted'
              ORDER BY sequence DESC LIMIT 1",
            params![key.0, key.1, key.2, key.3, key.4, key.5, key.6],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(internal)?;
    if let Some((operation_ref, input_digest, refusal_code, refusal_revision, envelope)) = refusal {
        if refusal_code.is_empty() {
            return Err(intent_corrupt("the retained refusal is incomplete"));
        }
        // A refused row is retained evidence too: it must be the closed
        // envelope over this namespace before it can report not_accepted.
        intent_envelope(namespace, &input_digest, &envelope)?;
        return Ok(IntentLookup::NotAccepted {
            operation_ref: intent_operation_ref(&operation_ref)?,
            input_digest,
            refusal_code,
            durable_refusal_revision: u64::try_from(refusal_revision)
                .map_err(|_| intent_corrupt("the retained refusal revision is invalid"))?,
        });
    }
    let watermark: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM delegation_operations",
            [],
            |row| row.get(0),
        )
        .map_err(internal)?;
    Ok(IntentLookup::Absent {
        durable_watermark: u64::try_from(watermark)
            .map_err(|_| intent_corrupt("the durable watermark is invalid"))?,
    })
}

/// The accepted member one successor spawn intent admitted, if it accepted here.
///
/// A successor managed member is identified by its accepted spawn intent,
/// on top of the successor marker its Broker row carries.
pub(crate) fn successor_spawn_member(
    connection: &Connection,
    operation_ref: &str,
) -> Result<Option<String>, MachineError> {
    let subjects: Option<String> = connection
        .query_row(
            "SELECT accepted_subjects_json FROM delegation_operations
              WHERE operation_ref=?1 AND operation_kind='spawn_specialist' AND state='accepted'",
            [operation_ref],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?;
    let Some(subjects) = subjects else {
        return Ok(None);
    };
    let subjects: AcceptedSubjects = serde_json::from_str(&subjects)
        .map_err(|_| intent_corrupt("the accepted subjects are invalid"))?;
    match subjects.member_ref {
        Some(member) => Ok(Some(member)),
        None => Err(intent_corrupt("the accepted spawn names no member")),
    }
}

/// Whether one external member was admitted by a successor hire.
///
/// A legacy facade-v4 hire leaves no successor intent, so only an accepted
/// `hire_external_specialist` subject names a successor member; that fact is
/// what separates the two release owners.
// The release settlement is constructed by the semantic release owner.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn successor_hire_member(
    connection: &Connection,
    member_ref: &str,
) -> Result<bool, MachineError> {
    let mut statement = connection
        .prepare(
            "SELECT accepted_subjects_json FROM delegation_operations
              WHERE operation_kind='hire_external_specialist' AND state='accepted'",
        )
        .map_err(internal)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    for subjects in rows {
        let subjects: AcceptedSubjects = serde_json::from_str(&subjects)
            .map_err(|_| intent_corrupt("the accepted subjects are invalid"))?;
        if subjects.member_ref.as_deref() == Some(member_ref) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The scoped membership revision a successor acceptance fences (RPC §3).
pub(crate) fn scope_membership_revision(
    connection: &Connection,
    workspace_ref: &str,
    scope_kind: &str,
    scope_ref: &str,
) -> Result<u64, MachineError> {
    let revision: Option<i64> = connection
        .query_row(
            "SELECT revision FROM scope_membership_revisions
              WHERE workspace_ref=?1 AND scope_kind=?2 AND scope_ref=?3",
            params![workspace_ref, scope_kind, scope_ref],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?;
    match revision {
        Some(revision) => {
            u64::try_from(revision).map_err(|_| intent_corrupt("the scope revision is invalid"))
        }
        None => Ok(0),
    }
}

/// One member's workload revision, zero before its first change.
pub(crate) fn member_workload_revision(
    connection: &Connection,
    workspace_ref: &str,
    scope_kind: &str,
    scope_ref: &str,
    member_ref: &str,
) -> Result<u64, MachineError> {
    let revision: Option<i64> = connection
        .query_row(
            "SELECT revision FROM member_workload_revisions
              WHERE workspace_ref=?1 AND scope_kind=?2 AND scope_ref=?3 AND member_ref=?4",
            params![workspace_ref, scope_kind, scope_ref, member_ref],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?;
    match revision {
        Some(revision) => {
            u64::try_from(revision).map_err(|_| intent_corrupt("the workload revision is invalid"))
        }
        None => Ok(0),
    }
}

/// Advance one member's workload revision inside the accepting transaction.
pub(crate) fn advance_member_workload_revision(
    transaction: &Transaction<'_>,
    workspace_ref: &str,
    scope_kind: &str,
    scope_ref: &str,
    member_ref: &str,
) -> Result<u64, MachineError> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO member_workload_revisions(
               workspace_ref,scope_kind,scope_ref,member_ref,revision)
             VALUES(?1,?2,?3,?4,0)",
            params![workspace_ref, scope_kind, scope_ref, member_ref],
        )
        .map_err(internal)?;
    transaction
        .execute(
            "UPDATE member_workload_revisions SET revision=revision+1
              WHERE workspace_ref=?1 AND scope_kind=?2 AND scope_ref=?3 AND member_ref=?4",
            params![workspace_ref, scope_kind, scope_ref, member_ref],
        )
        .map_err(internal)?;
    member_workload_revision(
        transaction,
        workspace_ref,
        scope_kind,
        scope_ref,
        member_ref,
    )
}

/// Advance one scoped membership revision inside the accepting transaction.
pub(crate) fn advance_scope_membership_revision(
    transaction: &Transaction<'_>,
    workspace_ref: &str,
    scope_kind: &str,
    scope_ref: &str,
) -> Result<u64, MachineError> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO scope_membership_revisions(workspace_ref,scope_kind,scope_ref,revision)
             VALUES(?1,?2,?3,0)",
            params![workspace_ref, scope_kind, scope_ref],
        )
        .map_err(internal)?;
    transaction
        .execute(
            "UPDATE scope_membership_revisions SET revision=revision+1
              WHERE workspace_ref=?1 AND scope_kind=?2 AND scope_ref=?3",
            params![workspace_ref, scope_kind, scope_ref],
        )
        .map_err(internal)?;
    scope_membership_revision(transaction, workspace_ref, scope_kind, scope_ref)
}

/// The durable release operation of one member, if one was admitted.
///
/// Anchored by `(workspace_ref, scope_kind, scope_ref, member_ref)` because the
/// `S` release owners carry no idempotency key (RPC:219-241, RPC §7).
// The release owner is constructed by the semantic release admission.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn member_release_operation(
    connection: &Connection,
    workspace_ref: &str,
    scope_kind: &str,
    scope_ref: &str,
    member_ref: &str,
) -> Result<Option<crate::delegation_identity::MemberReleaseOperation>, MachineError> {
    type Row = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        i64,
        Option<String>,
        String,
        Option<String>,
        String,
        Option<i64>,
        Option<i64>,
    );
    let row: Option<Row> = connection
        .query_row(
            "SELECT operation_ref,operation_kind,workspace_ref,scope_kind,scope_ref,
                    member_ref,run_ref,controller_principal,controller_generation,
                    admitted_membership_revision,owner_ref,progress,safe_failure_code,
                    required_action,terminal_membership_revision,settled_workload_revision
               FROM member_release_operations
              WHERE workspace_ref=?1 AND scope_kind=?2 AND scope_ref=?3 AND member_ref=?4",
            params![workspace_ref, scope_kind, scope_ref, member_ref],
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
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                ))
            },
        )
        .optional()
        .map_err(internal)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let unsigned = |value: i64, reason: &str| -> Result<u64, MachineError> {
        u64::try_from(value).map_err(|_| integrity(reason))
    };
    let operation = crate::delegation_identity::MemberReleaseOperation {
        schema: crate::delegation_identity::MEMBER_RELEASE_OPERATION_SCHEMA.to_owned(),
        operation_ref: row.0,
        operation_kind: row.1,
        workspace_ref: row.2,
        scope_kind: row.3,
        scope_ref: row.4,
        member_ref: row.5,
        run_ref: row.6,
        controller_principal: row.7,
        controller_generation: unsigned(row.8, "the release Controller generation is invalid")?,
        admitted_membership_revision: unsigned(row.9, "the release admission revision is invalid")?,
        owner_ref: row.10,
        progress: row.11,
        safe_failure_code: row.12,
        required_action: row.13,
        terminal_membership_revision: row
            .14
            .map(|value| unsigned(value, "the release terminal revision is invalid"))
            .transpose()?,
        settled_workload_revision: row
            .15
            .map(|value| unsigned(value, "the release workload revision is invalid"))
            .transpose()?,
    };
    operation
        .validate()
        .map_err(|_| integrity("the retained release operation is invalid"))?;
    Ok(Some(operation))
}

/// One durable release admission (RPC:242-264).
// The release owner is constructed by the semantic release admission.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct MemberReleaseAdmission<'a> {
    pub operation_ref: Uuid,
    pub workspace_ref: &'a str,
    pub scope_kind: &'a str,
    pub scope_ref: &'a str,
    pub member_ref: Uuid,
    pub run_ref: Uuid,
    pub controller_principal: &'a str,
    pub controller_generation: u64,
    pub admitted_membership_revision: u64,
    pub now_ms: i64,
}

/// Insert one `requested` release row inside the admitting transaction.
// The release owner is constructed by the semantic release admission.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn admit_member_release(
    transaction: &Transaction<'_>,
    admission: MemberReleaseAdmission<'_>,
) -> Result<crate::delegation_identity::MemberReleaseOperation, MachineError> {
    let operation_kind = match admission.scope_kind {
        "external_engagement" => "release_external_specialist",
        "orchestrated_session" => "release_specialist",
        _ => return Err(integrity("the release scope kind is not a member owner")),
    };
    if admission.controller_generation == 0
        || admission.controller_generation > crate::domain::MAX_JCS_SAFE_INTEGER
        || admission.admitted_membership_revision > crate::domain::MAX_JCS_SAFE_INTEGER
    {
        return Err(integrity("the release admission bounds are invalid"));
    }
    transaction
        .execute(
            "INSERT INTO member_release_operations(
               operation_ref,operation_kind,workspace_ref,scope_kind,scope_ref,
               member_ref,run_ref,controller_principal,controller_generation,
               admitted_membership_revision,owner_ref,progress,safe_failure_code,
               required_action,terminal_membership_revision,settled_workload_revision,
               created_at_ms,updated_at_ms
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,NULL,'requested',NULL,'none',
                      NULL,NULL,?11,?11)",
            params![
                admission.operation_ref.to_string(),
                operation_kind,
                admission.workspace_ref,
                admission.scope_kind,
                admission.scope_ref,
                admission.member_ref.to_string(),
                admission.run_ref.to_string(),
                admission.controller_principal,
                i64::try_from(admission.controller_generation)
                    .map_err(|_| integrity("the release Controller generation is invalid"))?,
                i64::try_from(admission.admitted_membership_revision)
                    .map_err(|_| integrity("the release admission revision is invalid"))?,
                admission.now_ms,
            ],
        )
        .map_err(internal)?;
    member_release_operation(
        transaction,
        admission.workspace_ref,
        admission.scope_kind,
        admission.scope_ref,
        &admission.member_ref.to_string(),
    )?
    .ok_or_else(|| integrity("the admitted release operation is missing"))
}

/// Insert one `requested` release row inside the admitting transaction, or
/// converge on a concurrent admission's row (RPC:433-456).
///
/// The `S` release has no idempotency key, so two callers that both read no
/// row can race into this transaction; the later one joins the committed
/// operation instead of fencing or colliding on the member's uniqueness.
// The release settlement is constructed by the semantic release owner.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn admit_or_join_member_release(
    transaction: &Transaction<'_>,
    admission: MemberReleaseAdmission<'_>,
) -> Result<crate::delegation_identity::MemberReleaseOperation, MachineError> {
    if let Some(existing) = member_release_operation(
        transaction,
        admission.workspace_ref,
        admission.scope_kind,
        admission.scope_ref,
        &admission.member_ref.to_string(),
    )? {
        return Ok(existing);
    }
    admit_member_release(transaction, admission)
}

/// The typed progress one owner attempt records on a release row.
// The release owner is constructed by the semantic release admission.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum MemberReleaseProgress<'a> {
    /// This attempt holds the member Run lease and now owns settlement.
    Owned { owner_ref: &'a str },
    /// This attempt could not continue; completion revisions stay null.
    Failed {
        progress: &'a str,
        safe_failure_code: &'a str,
        required_action: &'a str,
    },
}

/// Record one release progress transition inside its own transaction.
///
/// The row's identity, initiating Controller and admission revision are never
/// written here, and a `terminal` row refuses every transition
/// (RPC:296-301, :433-456).
// The release owner is constructed by the semantic release admission.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn record_member_release_progress(
    transaction: &Transaction<'_>,
    operation_ref: Uuid,
    progress: MemberReleaseProgress<'_>,
    now_ms: i64,
) -> Result<crate::delegation_identity::MemberReleaseOperation, MachineError> {
    let changed = match progress {
        MemberReleaseProgress::Owned { owner_ref } => {
            if owner_ref.is_empty() || !owner_ref.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(integrity(
                    "the release owner reference is not a witness digest",
                ));
            }
            transaction
                .execute(
                    "UPDATE member_release_operations
                        SET owner_ref=?2,progress='settling',safe_failure_code=NULL,
                            required_action='none',updated_at_ms=?3
                      WHERE operation_ref=?1 AND progress<>'terminal'",
                    params![operation_ref.to_string(), owner_ref, now_ms],
                )
                .map_err(internal)?
        }
        MemberReleaseProgress::Failed {
            progress,
            safe_failure_code,
            required_action,
        } => {
            if !matches!(progress, "cleanup_failed" | "recovery_needed") {
                return Err(integrity(
                    "the release failure progress is not a recorded state",
                ));
            }
            if !matches!(
                required_action,
                "reconcile_operation" | "recover_run" | "restore_state"
            ) {
                return Err(integrity(
                    "the release failure action is not a closed action",
                ));
            }
            if safe_failure_code.is_empty()
                || !safe_failure_code
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte == b'_' || byte.is_ascii_digit())
            {
                return Err(integrity("the release failure code is not a safe code"));
            }
            transaction
                .execute(
                    "UPDATE member_release_operations
                        SET progress=?2,safe_failure_code=?3,required_action=?4,
                            terminal_membership_revision=NULL,settled_workload_revision=NULL,
                            updated_at_ms=?5
                      WHERE operation_ref=?1 AND progress<>'terminal'",
                    params![
                        operation_ref.to_string(),
                        progress,
                        safe_failure_code,
                        required_action,
                        now_ms
                    ],
                )
                .map_err(internal)?
        }
    };
    if changed != 1 {
        return Err(conflict("the release operation cannot advance"));
    }
    let row: (String, String, String, String) = transaction
        .query_row(
            "SELECT workspace_ref,scope_kind,scope_ref,member_ref
               FROM member_release_operations WHERE operation_ref=?1",
            [operation_ref.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(internal)?;
    member_release_operation(transaction, &row.0, &row.1, &row.2, &row.3)?
        .ok_or_else(|| integrity("the advanced release operation is missing"))
}

/// Mark one external member `releasing` inside the admission transaction.
// The release owner is constructed by the semantic release admission.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn mark_external_member_releasing(
    transaction: &Transaction<'_>,
    engagement_id: Uuid,
    member_ref: Uuid,
    now_ms: i64,
) -> Result<(), MachineError> {
    let changed = transaction
        .execute(
            "UPDATE members SET state='releasing',updated_at_ms=?3
              WHERE engagement_id=?1 AND specialist_run_id=?2
                AND state NOT IN ('released','retired')",
            params![engagement_id.to_string(), member_ref.to_string(), now_ms],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Err(conflict("the member cannot enter release"));
    }
    append_event(
        transaction,
        engagement_id,
        "member_release_requested",
        &member_ref.to_string(),
    )
}

/// Transition one external member to terminally `released` inside the release
/// terminal transaction (RPC:388-394).
// The release settlement is constructed by the semantic release owner.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn retire_external_member_released(
    transaction: &Transaction<'_>,
    engagement_id: Uuid,
    member_ref: Uuid,
    now_ms: i64,
) -> Result<(), MachineError> {
    let changed = transaction
        .execute(
            "UPDATE members SET state='released',actor_residency='terminal',updated_at_ms=?3
              WHERE engagement_id=?1 AND specialist_run_id=?2
                AND state IN ('releasing','active','degraded')",
            params![engagement_id.to_string(), member_ref.to_string(), now_ms],
        )
        .map_err(internal)?;
    if changed == 0 {
        let existing: Option<String> = transaction
            .query_row(
                "SELECT state FROM members WHERE engagement_id=?1 AND specialist_run_id=?2",
                params![engagement_id.to_string(), member_ref.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(internal)?;
        if existing.as_deref() != Some("released") {
            return Err(conflict("the member cannot be released"));
        }
    }
    append_event(
        transaction,
        engagement_id,
        "external_member_released",
        &member_ref.to_string(),
    )
}

/// Freeze one release operation at `terminal` with both completion revisions
/// inside the same transaction that settles the member (RPC:388-394).
// The release settlement is constructed by the semantic release owner.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn commit_member_release_terminal(
    transaction: &Transaction<'_>,
    operation_ref: Uuid,
    terminal_membership_revision: u64,
    settled_workload_revision: u64,
    now_ms: i64,
) -> Result<crate::delegation_identity::MemberReleaseOperation, MachineError> {
    if terminal_membership_revision > crate::domain::MAX_JCS_SAFE_INTEGER
        || settled_workload_revision > crate::domain::MAX_JCS_SAFE_INTEGER
    {
        return Err(integrity("the release completion revisions are invalid"));
    }
    let changed = transaction
        .execute(
            "UPDATE member_release_operations
                SET progress='terminal',terminal_membership_revision=?2,
                    settled_workload_revision=?3,safe_failure_code=NULL,
                    required_action='none',updated_at_ms=?4
              WHERE operation_ref=?1 AND progress<>'terminal'",
            params![
                operation_ref.to_string(),
                i64::try_from(terminal_membership_revision)
                    .map_err(|_| integrity("the release terminal revision is invalid"))?,
                i64::try_from(settled_workload_revision)
                    .map_err(|_| integrity("the release workload revision is invalid"))?,
                now_ms
            ],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Err(conflict("the release operation cannot settle"));
    }
    let row: (String, String, String, String) = transaction
        .query_row(
            "SELECT workspace_ref,scope_kind,scope_ref,member_ref
               FROM member_release_operations WHERE operation_ref=?1",
            [operation_ref.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(internal)?;
    member_release_operation(transaction, &row.0, &row.1, &row.2, &row.3)?
        .ok_or_else(|| integrity("the settled release operation is missing"))
}

fn intent_operation_ref(value: &str) -> Result<Uuid, MachineError> {
    Uuid::parse_str(value)
        .map_err(|_| intent_corrupt("the retained operation reference is invalid"))
}

fn intent_subjects(value: &str) -> Result<AcceptedSubjects, MachineError> {
    serde_json::from_str(value).map_err(|_| intent_corrupt("the retained subjects are invalid"))
}

/// Rebuild one accepted receipt and bind it to its retained intent row.
fn intent_receipt(
    namespace: &LookupNamespace,
    operation_ref: Uuid,
    digest: &str,
    envelope_json: &str,
    receipt_json: Option<&str>,
    subjects_json: &str,
) -> Result<OperationReceipt, MachineError> {
    intent_envelope(namespace, digest, envelope_json)?;
    let receipt_json =
        receipt_json.ok_or_else(|| intent_corrupt("the accepted receipt is missing"))?;
    let receipt: OperationReceipt = serde_json::from_str(receipt_json)
        .map_err(|_| intent_corrupt("the retained receipt is malformed"))?;
    receipt
        .validate()
        .map_err(|_| intent_corrupt("the retained receipt is invalid"))?;
    receipt
        .verify_digest()
        .map_err(|_| intent_corrupt("the retained receipt digest does not verify"))?;
    if receipt.namespace != *namespace
        || receipt.operation_ref != operation_ref.to_string()
        || receipt.accepted_input_digest != digest
        || receipt.accepted_subjects != intent_subjects(subjects_json)?
    {
        return Err(intent_corrupt(
            "the retained receipt differs from its intent row",
        ));
    }
    Ok(receipt)
}

/// Require one retained mutation envelope to be the closed envelope over this
/// row's own namespace with the digest the row recorded.
fn intent_envelope(
    namespace: &LookupNamespace,
    digest: &str,
    envelope_json: &str,
) -> Result<(), MachineError> {
    // Parse through the repository JCS reader first: it rejects duplicate
    // object keys and non-canonical input, which a plain JSON reader would
    // silently collapse (MDC:212-215).
    let parsed = crate::jcs::parse(envelope_json)
        .map_err(|_| intent_corrupt("the retained mutation envelope is not canonical JSON"))?;
    let canonical = crate::jcs::canonicalize(&parsed)
        .map_err(|_| intent_corrupt("the retained mutation envelope cannot be re-encoded"))?;
    let envelope: crate::operation::Envelope = serde_json::from_slice(&canonical)
        .map_err(|_| intent_corrupt("the retained mutation envelope is not the closed envelope"))?;
    if envelope.schema != crate::operation::ENVELOPE_SCHEMA
        || LookupNamespace::from(&envelope.namespace) != *namespace
        || crate::operation::envelope_digest(&envelope)
            .map_err(|_| intent_corrupt("the retained mutation envelope cannot be re-digested"))?
            != digest
    {
        return Err(intent_corrupt(
            "the retained mutation envelope differs from its intent row",
        ));
    }
    Ok(())
}

/// The accepted-input digest of the latest durable refusal for one key.
///
/// The refused row is verified like every other retained intent before it can
/// become a re-entry basis: a corrupt refusal is recovery, never a comparison.
fn latest_refusal_digest(
    connection: &Connection,
    namespace: &LookupNamespace,
) -> Result<Option<String>, MachineError> {
    let key = intent_key(namespace);
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT mutation_envelope_digest,mutation_envelope_json FROM delegation_operations
              WHERE workspace_ref=?1 AND controller_principal=?2 AND controller_generation=?3
                AND scope_kind IS ?4 AND scope_ref IS ?5 AND operation_kind=?6
                AND idempotency_key=?7 AND state='resolved_not_accepted'
              ORDER BY sequence DESC LIMIT 1",
            params![key.0, key.1, key.2, key.3, key.4, key.5, key.6],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((digest, envelope)) = row else {
        return Ok(None);
    };
    intent_envelope(namespace, &digest, &envelope)?;
    Ok(Some(digest))
}

/// A key that a legacy facade-v4 path owns is never absent and never new.
fn legacy_operation_collision(
    connection: &Connection,
    namespace: &LookupNamespace,
) -> Result<(), MachineError> {
    let Some((scope_id, operation)) = legacy_key_scope(namespace) else {
        return Ok(());
    };
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM operations
              WHERE scope_id=?1 AND operation=?2 AND idempotency_key=?3)",
            params![scope_id, operation, namespace.idempotency_key],
            |row| row.get(0),
        )
        .map_err(internal)?;
    if exists {
        return Err(legacy_namespace_refusal());
    }
    Ok(())
}

/// The legacy `operations` scope one successor key would have used, if any.
fn legacy_key_scope(namespace: &LookupNamespace) -> Option<(String, &'static str)> {
    let engagement_scope = || match (&namespace.scope_kind, &namespace.scope_ref) {
        (Some(kind), Some(scope)) if kind == "external_engagement" => Some(scope.clone()),
        _ => None,
    };
    match namespace.operation_kind.as_str() {
        "open_external_engagement" => Some((
            format!("workspace:{}", namespace.workspace_ref),
            "open_external_engagement",
        )),
        "hire_external_specialist" => {
            engagement_scope().map(|scope| (scope, "hire_external_specialist"))
        }
        "assign_external_specialist_task" => {
            engagement_scope().map(|scope| (scope, "assign_external_specialist_task"))
        }
        _ => None,
    }
}

fn legacy_namespace_refusal() -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "the idempotency key belongs to the legacy operation namespace",
        false,
        serde_json::json!({
            "subject":"legacy_operation_namespace",
            "required_action":"reconcile_operation",
            "effect":"none"
        }),
    )
}

fn intent_conflict() -> MachineError {
    MachineError::new(
        "IDEMPOTENCY_CONFLICT",
        "the idempotency key already names a different delegated operation",
        false,
        serde_json::json!({"required_action":"use_original_input_or_new_key"}),
    )
}

fn intent_corrupt(reason: &str) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        reason,
        false,
        serde_json::json!({
            "subject":"delegation_operations",
            "required_action":"restore_state",
            "effect":"none"
        }),
    )
}

fn intent_constraint(error: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;
    matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::ConstraintViolation,
                ..
            },
            _
        )
    )
}

fn read_birth_record(path: &Path) -> Result<Option<Uuid>, MachineError> {
    let bytes = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(namespace_record_corrupt("the birth record is unreadable")),
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(namespace_record_corrupt(
                "the namespace birth record is not a regular file",
            ));
        }
        Ok(_) => std::fs::read(path)
            .map_err(|_| namespace_record_corrupt("the namespace birth record cannot be read"))?,
    };
    let record: NamespaceBirth = serde_json::from_slice(&bytes)
        .map_err(|_| namespace_record_corrupt("the namespace birth record is malformed"))?;
    if record.schema != NAMESPACE_BIRTH_SCHEMA {
        return Err(namespace_record_corrupt(
            "the namespace birth record has an unknown schema",
        ));
    }
    Uuid::parse_str(&record.namespace_birth_id)
        .map(Some)
        .map_err(|_| namespace_record_corrupt("the namespace birth id is not a UUID"))
}

/// Publish the birth record once.
///
/// A mismatching existing record is never overwritten: the namespace identity
/// must not be replaced silently. A concurrent opener that published the same
/// id first is convergence, not failure.
fn publish_birth_record(database: &Path, birth_id: Uuid) -> Result<(), MachineError> {
    match read_birth_record(&birth_record_path(database)?)? {
        Some(existing) if existing == birth_id => return Ok(()),
        Some(_) => return Err(namespace_record_mismatch()),
        None => {}
    }
    let path = birth_record_path(database)?;
    let parent = path.parent().ok_or_else(database_unavailable)?;
    std::fs::create_dir_all(parent).map_err(internal)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(internal)?;
    let record = NamespaceBirth {
        schema: NAMESPACE_BIRTH_SCHEMA.to_owned(),
        namespace_birth_id: birth_id.to_string(),
    };
    let mut bytes = crate::jcs::canonicalize(
        &crate::jcs::parse(&serde_json::to_string(&record).map_err(internal)?)
            .map_err(|_| database_unavailable())?,
    )
    .map_err(|_| database_unavailable())?;
    bytes.push(b'\n');
    match crate::workspace::atomic_create(
        &crate::workspace::SystemWorkspacePlatform,
        &path,
        &bytes,
        0o600,
    ) {
        Ok(()) => Ok(()),
        // Another opener won the exclusive publication. The same id is
        // convergence; a different id is a namespace conflict.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match read_birth_record(&path)? {
                Some(existing) if existing == birth_id => Ok(()),
                Some(_) => Err(namespace_record_mismatch()),
                None => Err(database_unavailable()),
            }
        }
        Err(error) => Err(internal(error)),
    }
}

/// The birth record that belongs to one Engagement database.
///
/// Production layout: `<state_root>/orchestration/namespace-birth.json`, beside
/// the database, which is exactly `namespace_birth_path`. Isolated/fixture
/// convention: `<database file name>.namespace-birth.json` in the database's own
/// directory (for example `orchestration.sqlite3.namespace-birth.json`), so two
/// databases in one directory can never be mistaken for one namespace. The
/// isolated name is **not** a production layout.
fn birth_record_path(database: &Path) -> Result<PathBuf, MachineError> {
    let parent = database.parent().ok_or_else(database_unavailable)?;
    if parent
        .file_name()
        .is_some_and(|name| name == "orchestration")
    {
        return Ok(parent.join(NAMESPACE_BIRTH_FILE));
    }
    let name = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(database_unavailable)?;
    Ok(parent.join(format!("{name}.{NAMESPACE_BIRTH_FILE}")))
}

/// A birth record that cannot be trusted is a recovery condition, never
/// authoritative absence.
fn namespace_record_corrupt(reason: &str) -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        reason,
        false,
        serde_json::json!({"required_action":"restore_state","subject":"namespace_birth"}),
    )
}

/// Turn a created generation-6 database back into a genuine older shape.
///
/// Removing only `metadata.schema_version` leaves the generation-6 objects
/// in place, which is not a shape the runtime accepts. A real historical
/// database carries neither those tables nor a namespace birth id, and it has
/// no birth record beside it.
#[cfg(test)]
pub(crate) fn downgrade_to_legacy_for_test(
    database: &Path,
    connection: &Connection,
    generation: u8,
) {
    assert!((1..=5).contains(&generation));
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE IF EXISTS dispatch_indexes;
             DROP TABLE IF EXISTS member_release_operations;
             DROP TABLE IF EXISTS member_workload_revisions;
             DROP TABLE IF EXISTS scope_membership_revisions;
             DROP TABLE IF EXISTS caller_scope_assignments;
             DROP TABLE IF EXISTS delegation_operations;
             DELETE FROM metadata WHERE key='namespace_birth_id';",
        )
        .unwrap();
    connection
        .execute(
            "UPDATE metadata SET value=?1 WHERE key='schema_version'",
            [generation.to_string()],
        )
        .unwrap();
    // The fixture mutation must be durable before the sidecars are cleared,
    // and the namespace record must not survive an older shape.
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    if let Ok(path) = birth_record_path(database) {
        let _ = std::fs::remove_file(path);
    }
}

/// The namespace birth id a generation-6 database must carry.
fn probe_birth_id(probe: &GenerationProbe) -> Result<Uuid, MachineError> {
    probe
        .namespace_birth_id
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| {
            MachineError::new(
                "RECOVERY_REQUIRED",
                "the generation-6 orchestration database has no namespace birth id",
                false,
                serde_json::json!({"required_action":"restore_state","subject":"namespace_birth"}),
            )
        })
}

/// A birth record without the database it describes can never be repaired by
/// creating a new namespace.
fn namespace_record_without_database() -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "the namespace birth record has no database",
        false,
        serde_json::json!({"required_action":"restore_state","subject":"namespace_birth"}),
    )
}

/// A birth record that does not describe this database.
fn namespace_record_mismatch() -> MachineError {
    MachineError::new(
        "RECOVERY_REQUIRED",
        "the namespace birth record names another namespace",
        false,
        serde_json::json!({"required_action":"restore_state","subject":"namespace_birth"}),
    )
}

fn fresh_birth_id() -> Uuid {
    Uuid::now_v7()
}

fn migrate_v5_to_v6(
    connection: &mut Connection,
    birth_id: Uuid,
    faults: &Arc<dyn EngagementFaultInjector>,
) -> Result<(), MachineError> {
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
    if !["5", SCHEMA_VERSION.to_string().as_str()].contains(&observed.as_str()) {
        return Err(MachineError::new(
            "ORCHESTRATION_SCHEMA_UNSUPPORTED",
            "orchestration schema is not supported",
            false,
            serde_json::json!({"observed":observed}),
        ));
    }
    if observed == "5" {
        // A generation-5 database that already carries a generation-6 object is
        // not a verified shape: refuse before the DDL so the caller gets a typed
        // schema refusal instead of a failed `CREATE TABLE`.
        let carried: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN (
                   'delegation_operations','caller_scope_assignments',
                   'scope_membership_revisions','member_workload_revisions',
                   'member_release_operations','dispatch_indexes')",
                [],
                |row| row.get(0),
            )
            .map_err(internal)?;
        if carried != 0 {
            return Err(MachineError::new(
                "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                "orchestration database carries generation-6 objects but claims generation 5",
                false,
                serde_json::json!({"observed":observed}),
            ));
        }
        crate::storage_observation::count_migration();
        transaction
            .execute_batch(GENERATION_SIX_DDL)
            .map_err(internal)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO metadata(key,value) VALUES('namespace_birth_id',?1)",
                [birth_id.to_string()],
            )
            .map_err(internal)?;
        transaction
            .execute(
                "UPDATE metadata SET value=?1 WHERE key='schema_version'",
                [SCHEMA_VERSION.to_string()],
            )
            .map_err(internal)?;
    }
    faults
        .check(EngagementBarrier::BeforeMigrationV6Commit)
        .map_err(internal)?;
    crate::storage_observation::commit(transaction).map_err(internal)?;
    faults
        .check(EngagementBarrier::AfterMigrationV6Commit)
        .map_err(internal)
}

/// Read the accepted birth id of a generation-6 database.
fn database_birth_id(connection: &Connection) -> Result<Uuid, MachineError> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key='namespace_birth_id'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(internal)?;
    value
        .and_then(|value| Uuid::parse_str(&value).ok())
        .ok_or_else(|| {
            MachineError::new(
                "RECOVERY_REQUIRED",
                "the orchestration database has no namespace birth id",
                false,
                serde_json::json!({"required_action":"restore_state"}),
            )
        })
}

fn migrate_v1_to_v2(connection: &mut Connection) -> Result<(), MachineError> {
    // Foreign-key enforcement is connection state, not transaction state, and
    // this connection is pooled: it is restored on every exit path.
    let result = migrate_v1_to_v2_inner(connection);
    let restored = connection
        .execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(internal);
    result.and(restored)
}

fn migrate_v1_to_v2_inner(connection: &mut Connection) -> Result<(), MachineError> {
    // Generation 1 renamed and rebuilt its artifact and receipt pair. The
    // accepted migration preserves every historical row and revalidates
    // foreign keys before it commits.
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
    } else if !["2", "3", "4", "5", "6"].contains(&locked_observed.as_str()) {
        return Err(MachineError::new(
            "ORCHESTRATION_SCHEMA_UNSUPPORTED",
            "orchestration schema is not supported",
            false,
            serde_json::json!({"observed":locked_observed}),
        ));
    }
    crate::storage_observation::commit(transaction).map_err(internal)?;
    Ok(())
}

fn migrate_v2_to_v3(connection: &mut Connection) -> Result<(), MachineError> {
    // See `migrate_v1_to_v2`: the pooled connection restores the pragma on every
    // exit path.
    let result = migrate_v2_to_v3_inner(connection);
    let restored = connection
        .execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(internal);
    result.and(restored)
}

fn migrate_v2_to_v3_inner(connection: &mut Connection) -> Result<(), MachineError> {
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
    } else if !["3", "4", "5", "6"].contains(&observed.as_str()) {
        return Err(MachineError::new(
            "ORCHESTRATION_SCHEMA_UNSUPPORTED",
            "orchestration schema is not supported",
            false,
            serde_json::json!({"observed":observed}),
        ));
    }
    crate::storage_observation::commit(transaction).map_err(internal)
}

fn migrate_v3_to_v4(connection: &mut Connection) -> Result<(), MachineError> {
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
    if observed == "3" {
        transaction
            .execute_batch(
                "ALTER TABLE tasks ADD COLUMN diagnostic_json TEXT;
             ALTER TABLE delivery_receipts RENAME TO delivery_receipts_v3;
             CREATE TABLE delivery_receipts(
               sequence INTEGER PRIMARY KEY AUTOINCREMENT,
               task_id TEXT NOT NULL UNIQUE, artifact_id TEXT, delivered_at_ms INTEGER NOT NULL,
               FOREIGN KEY(task_id) REFERENCES tasks(task_id),
               FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
             ) STRICT;
             INSERT INTO delivery_receipts SELECT * FROM delivery_receipts_v3;
             DROP TABLE delivery_receipts_v3;
             UPDATE metadata SET value='4' WHERE key='schema_version';",
            )
            .map_err(internal)?;
    } else if !["4", "5", "6"].contains(&observed.as_str()) {
        return Err(MachineError::new(
            "ORCHESTRATION_SCHEMA_UNSUPPORTED",
            "orchestration schema is not supported",
            false,
            serde_json::json!({"observed":observed}),
        ));
    }
    crate::storage_observation::commit(transaction).map_err(internal)
}

fn migrate_v4_to_v5(connection: &mut Connection) -> Result<(), MachineError> {
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
    if !["4", "5", "6"].contains(&observed.as_str()) {
        return Err(MachineError::new(
            "ORCHESTRATION_SCHEMA_UNSUPPORTED",
            "orchestration schema is not supported",
            false,
            serde_json::json!({"observed":observed}),
        ));
    }
    // Generation 6 already carries this step's objects. A fresh bootstrap is
    // born at generation 5 without them, so "5" still runs the step.
    if observed == "6" {
        crate::storage_observation::commit(transaction).map_err(internal)?;
        return Ok(());
    }
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS one_shot_operations(
           request_ref TEXT PRIMARY KEY,
           receipt_json TEXT NOT NULL,
           receipt_sha256 TEXT NOT NULL,
           engagement_id TEXT UNIQUE,
           FOREIGN KEY(engagement_id) REFERENCES engagements(engagement_id)
         ) STRICT;
         UPDATE metadata SET value='5' WHERE key='schema_version';",
        )
        .map_err(internal)?;
    crate::storage_observation::commit(transaction).map_err(internal)
}

fn validate_one_shot_input(input: &OneShotOperationInput) -> Result<(), MachineError> {
    if input.request_ref.get_version_num() != 7 {
        return Err(invalid("request_ref", "one-shot reference must be UUIDv7"));
    }
    checked(&input.workspace_id, 256, "workspace_id")?;
    checked(&input.profile, 128, "profile")?;
    digest(
        &input.recovery_controller.capability_sha256,
        "capability_sha256",
    )?;
    if !input.carrier_root.is_absolute() || input.carrier_root.to_str().is_none() {
        return Err(invalid(
            "carrier_root",
            "one-shot carrier root must be an absolute UTF-8 path",
        ));
    }
    if !input.request.is_object() || canonical_json(&input.request)?.len() > 1_048_576 {
        return Err(invalid(
            "request",
            "one-shot request must be an object of at most 1048576 bytes",
        ));
    }
    Ok(())
}

fn one_shot_conflict() -> MachineError {
    MachineError::new(
        "IDEMPOTENCY_CONFLICT",
        "one-shot reference or binding was already used with different input",
        false,
        serde_json::json!({"required_action":"use_original_input_or_new_key"}),
    )
}

fn bind_once<T: PartialEq>(slot: &mut Option<T>, value: T) -> Result<(), MachineError> {
    match slot {
        Some(existing) if *existing != value => Err(one_shot_conflict()),
        Some(_) => Ok(()),
        None => {
            *slot = Some(value);
            Ok(())
        }
    }
}

fn one_shot_in(
    connection: &Connection,
    request_ref: Uuid,
) -> Result<Option<OneShotOperation>, MachineError> {
    let table_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='one_shot_operations')",
        [], |row| row.get(0),
    ).map_err(internal)?;
    if !table_exists {
        return Ok(None);
    }
    let row: Option<(String, String, Option<String>)> = connection.query_row(
        "SELECT receipt_json,receipt_sha256,engagement_id FROM one_shot_operations WHERE request_ref=?1",
        [request_ref.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(internal)?;
    let Some((json, digest, engagement_id)) = row else {
        return Ok(None);
    };
    if sha256_hex(json.as_bytes()) != digest {
        return Err(integrity("one-shot receipt digest does not match"));
    }
    let operation: OneShotOperation =
        serde_json::from_str(&json).map_err(|_| integrity("one-shot receipt is invalid"))?;
    validate_one_shot_input(&operation.input)?;
    if operation.schema != "dolgorae-one-shot-operation/v1"
        || operation.input.request_ref != request_ref
        || operation.request_sha256 != digest_value(&operation.input.request)?
        || operation.input_sha256
            != digest_value(&serde_json::to_value(&operation.input).map_err(internal)?)?
        || operation.engagement_id.map(|id| id.to_string()) != engagement_id
    {
        return Err(integrity("one-shot receipt binding does not match"));
    }
    Ok(Some(operation))
}

fn write_one_shot(
    connection: &Connection,
    operation: &OneShotOperation,
) -> Result<(), MachineError> {
    let json = canonical_string(&serde_json::to_value(operation).map_err(internal)?)?;
    let changed = connection.execute(
        "UPDATE one_shot_operations SET receipt_json=?2,receipt_sha256=?3,engagement_id=?4 WHERE request_ref=?1",
        params![operation.input.request_ref.to_string(), json, sha256_hex(json.as_bytes()), operation.engagement_id.map(|id| id.to_string())],
    ).map_err(internal)?;
    if changed != 1 {
        return Err(not_found());
    }
    Ok(())
}

fn commit_with(
    faults: &Arc<dyn EngagementFaultInjector>,
    transaction: Transaction<'_>,
    before: EngagementBarrier,
    after: EngagementBarrier,
) -> Result<(), MachineError> {
    faults.check(before).map_err(internal)?;
    crate::storage_observation::commit(transaction).map_err(internal)?;
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
    let (mut result, expected_digest) = connection
        .query_row(
            "SELECT t.specialist_run_id,t.state,t.turn_id,t.artifact_id,
                    t.safe_error_code,a.canonical_json,t.diagnostic_json,
                    t.request_json IS NOT NULL,a.engagement_id,a.task_id,a.specialist_run_id,
                    EXISTS(SELECT 1 FROM artifacts owned WHERE owned.task_id=t.task_id),
                    a.result_sha256,t.result_sha256
             FROM tasks t LEFT JOIN artifacts a ON a.artifact_id=t.artifact_id
             WHERE t.engagement_id=?1 AND t.task_id=?2",
            params![engagement_id.to_string(), task_id.to_string()],
            |row| {
                let specialist: String = row.get(0)?;
                let artifact: Option<String> = row.get(3)?;
                let state: String = row.get(1)?;
                let json: Option<String> = row.get(5)?;
                let native_request: bool = row.get(7)?;
                let artifact_engagement: Option<String> = row.get(8)?;
                let artifact_task: Option<String> = row.get(9)?;
                let artifact_specialist: Option<String> = row.get(10)?;
                let owned_artifact: bool = row.get(11)?;
                let artifact_digest: Option<String> = row.get(12)?;
                let task_digest: Option<String> = row.get(13)?;
                let result_bearing = state == "completed_not_delivered"
                    || (state == "delivered" && native_request)
                    || owned_artifact;
                if result_bearing && (artifact.is_none() || json.is_none()) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                if artifact.is_some()
                    && (json.is_none()
                        || artifact_engagement.as_deref() != Some(&engagement_id.to_string())
                        || artifact_task
                            .as_deref()
                            .is_some_and(|owner| owner != task_id.to_string())
                        || artifact_specialist
                            .as_deref()
                            .is_some_and(|owner| owner != specialist)
                        || (native_request
                            && (artifact_task.is_none()
                                || artifact_specialist.is_none()
                                || task_digest.is_none()))
                        || (task_digest.is_some() && task_digest != artifact_digest))
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                Ok((
                    ExternalTaskSnapshot {
                        task_id,
                        specialist_run_id: specialist
                            .parse()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        state,
                        turn_id: row.get(2)?,
                        result_artifact_ref: artifact
                            .map(|value| value.parse())
                            .transpose()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        result: json
                            .filter(|value| !value.is_empty())
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
                        diagnostic: row
                            .get::<_, Option<String>>(6)?
                            .map(|value| serde_json::from_str(&value))
                            .transpose()
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    6,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?,
                    },
                    artifact_digest,
                ))
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
        })?;
    if let Some(value) = &result.result
        && expected_digest.as_deref() != Some(digest_value(value)?.as_str())
    {
        return Err(integrity(
            "artifact JSON differs from its original result digest",
        ));
    }
    if result.result.is_none()
        && let Some(artifact) = result.result_artifact_ref
    {
        let reference = crate::storage_observation::content_reference(
            connection,
            "external_artifact",
            &artifact.to_string(),
            "json",
        )?
        .ok_or_else(|| integrity("normalized artifact reference is absent"))?;
        if expected_digest.as_deref() != Some(reference.sha256.as_str()) {
            return Err(integrity(
                "artifact reference differs from its original result digest",
            ));
        }
        match crate::storage_observation::read_referenced_content(connection, &reference) {
            Err(error) if error.code == "RESULT_CONTENT_UNAVAILABLE" => {}
            result_content => {
                let bytes = result_content?;
                result.result = Some(serde_json::from_slice(&bytes).map_err(internal)?);
            }
        }
    }
    Ok(result)
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

    // ---- generation 6 ------------------------------------------------------

    /// Build a workspace-shaped state root so `birth_record_path` and
    /// `database_admission` see the production layout.
    fn open_error(result: Result<EngagementStore, MachineError>) -> MachineError {
        match result {
            Ok(_) => panic!("expected a typed open failure"),
            Err(error) => error,
        }
    }

    fn generation_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("dolgorae-generation-{}", Uuid::now_v7()));
        std::fs::create_dir_all(root.join("state/orchestration")).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(root.join("state"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(
            root.join("state/orchestration"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        root
    }

    fn generation_database(root: &Path) -> std::path::PathBuf {
        root.join("state/orchestration/orchestration.sqlite3")
    }

    /// Create one historical generation from the committed birth fixtures.
    fn legacy_database(root: &Path, generation: u8) -> std::path::PathBuf {
        assert!((1..=5).contains(&generation));
        let fixture = match generation {
            1 => include_str!("../tests/fixtures/engagement-generations/base-1.sql"),
            2 => include_str!("../tests/fixtures/engagement-generations/base-2.sql"),
            3 => include_str!("../tests/fixtures/engagement-generations/base-3.sql"),
            4 => include_str!("../tests/fixtures/engagement-generations/base-4.sql"),
            _ => include_str!("../tests/fixtures/engagement-generations/base-5.sql"),
        };
        let path = generation_database(root);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(fixture).unwrap();
        connection
            .execute(
                "INSERT INTO metadata(key,value) VALUES('legacy_canary','keep')",
                [],
            )
            .unwrap();
        drop(connection);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    fn table_exists(connection: &Connection, name: &str) -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [name],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    }

    fn observed_generation(path: &Path) -> Option<String> {
        let connection =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        connection
            .query_row(
                "SELECT value FROM metadata WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    const GENERATION_SIX_TABLES: [&str; 6] = [
        "delegation_operations",
        "caller_scope_assignments",
        "scope_membership_revisions",
        "member_workload_revisions",
        "member_release_operations",
        "dispatch_indexes",
    ];

    #[test]
    fn generation_five_carrying_a_generation_six_object_fails_closed() {
        let root = generation_root();
        let path = legacy_database(&root, 5);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE dispatch_indexes(task_id TEXT PRIMARY KEY) STRICT;")
            .unwrap();
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(connection);
        let before = std::fs::read(&path).unwrap();
        let error = open_error(EngagementStore::open(&path));
        // A generation-5 database carrying a generation-6 object is refused
        // before the DDL, and the transaction rolls back with no partial schema.
        assert_eq!(error.code, "ORCHESTRATION_SCHEMA_UNSUPPORTED");
        assert_eq!(observed_generation(&path).as_deref(), Some("5"));
        let connection = Connection::open(&path).unwrap();
        for table in GENERATION_SIX_TABLES {
            if table == "dispatch_indexes" {
                // The pre-existing foreign object is the one the fixture created.
                continue;
            }
            assert!(!table_exists(&connection, table), "leaked {table}");
        }
        drop(connection);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!namespace_birth_path(&root.join("state")).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_birth_record_without_its_database_blocks_creation() {
        for empty_database in [false, true] {
            let root = generation_root();
            let state = root.join("state");
            let path = generation_database(&root);
            if empty_database {
                // A database whose bootstrap never committed carries no objects.
                std::fs::write(&path, b"").unwrap();
            }
            let record = format!(
                "{{\"namespace_birth_id\":\"{}\",\"schema\":\"dolgorae.namespace-birth/v1\"}}\n",
                Uuid::now_v7()
            );
            std::fs::create_dir_all(state.join("orchestration")).unwrap();
            std::fs::write(namespace_birth_path(&state), record.as_bytes()).unwrap();
            let error = open_error(EngagementStore::open(&path));
            assert_eq!(error.code, "RECOVERY_REQUIRED", "empty={empty_database}");
            assert_eq!(
                std::fs::read_to_string(namespace_birth_path(&state)).unwrap(),
                record
            );
            if empty_database {
                assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
            } else {
                assert!(!path.exists());
            }
            assert_eq!(
                delegation_namespace(&state).unwrap(),
                DelegationNamespace::RecoveryRequired
            );
            assert_eq!(
                ensure_delegation_namespace(&state).unwrap_err().code,
                "RECOVERY_REQUIRED"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn a_birth_record_blocks_migrating_a_legacy_database() {
        let root = generation_root();
        let state = root.join("state");
        let path = legacy_database(&root, 5);
        let before = std::fs::read(&path).unwrap();
        std::fs::write(
            namespace_birth_path(&state),
            format!(
                "{{\"namespace_birth_id\":\"{}\",\"schema\":\"dolgorae.namespace-birth/v1\"}}\n",
                Uuid::now_v7()
            ),
        )
        .unwrap();
        let error = open_error(EngagementStore::open(&path));
        assert_eq!(error.code, "RECOVERY_REQUIRED");
        assert_eq!(observed_generation(&path).as_deref(), Some("5"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            delegation_namespace(&state).unwrap(),
            DelegationNamespace::RecoveryRequired
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_legacy_database_is_verified_and_admitted_before_it_is_migrated() {
        // An insecure-mode legacy database is refused before any migration.
        let root = generation_root();
        let path = legacy_database(&root, 5);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let before = std::fs::read(&path).unwrap();
        let error = open_error(EngagementStore::open(&path));
        // The checked open verifies the file mode before any migration effect;
        // the raw secure-file check refuses a group-readable database.
        assert_eq!(error.code, "RUNTIME_PATH_INVALID");
        assert_eq!(
            error.details["reason"],
            "file must be current-uid-owned mode 0600 without symlink traversal"
        );
        assert_eq!(observed_generation(&path).as_deref(), Some("5"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!namespace_birth_path(&root.join("state")).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn probe_open_mode_covers_every_sidecar_state() {
        assert_eq!(probe_open_mode(SidecarState::Neither), ProbeOpen::Immutable);
        assert_eq!(probe_open_mode(SidecarState::Both), ProbeOpen::ReadOnly);
        assert_eq!(
            probe_open_mode(SidecarState::JournalOnly),
            ProbeOpen::ReadOnly
        );
        assert_eq!(
            probe_open_mode(SidecarState::LogOnly),
            ProbeOpen::RecoveryRequired
        );
        assert_eq!(
            probe_open_mode(SidecarState::MemoryOnly),
            ProbeOpen::RecoveryRequired
        );
    }

    #[test]
    fn a_transient_one_sided_sidecar_pair_is_re_observed_before_recovery() {
        let path = Path::new("/unused");
        // A close that unlinks `-shm` before deleting `-wal` is transient, not
        // recovery: a settled state is admitted.
        let mut script = vec![
            SidecarState::LogOnly,
            SidecarState::Neither,
            SidecarState::Neither,
        ]
        .into_iter();
        let settled = settle_sidecars(path, &mut |_| {
            Ok(script.next().unwrap_or(SidecarState::Neither))
        })
        .unwrap();
        assert_eq!(settled, SidecarState::Neither);
        assert_eq!(probe_open_mode(settled), ProbeOpen::Immutable);

        // An open that created `-wal` before `-shm` settles into a live pair.
        let mut script = vec![SidecarState::MemoryOnly, SidecarState::Both].into_iter();
        let settled = settle_sidecars(path, &mut |_| {
            Ok(script.next().unwrap_or(SidecarState::Both))
        })
        .unwrap();
        assert_eq!(settled, SidecarState::Both);
        assert_eq!(probe_open_mode(settled), ProbeOpen::ReadOnly);

        // A persistent one-sided pair stays a recovery condition.
        let mut observations = 0;
        let settled = settle_sidecars(path, &mut |_| {
            observations += 1;
            Ok(SidecarState::LogOnly)
        })
        .unwrap();
        assert_eq!(settled, SidecarState::LogOnly);
        assert_eq!(probe_open_mode(settled), ProbeOpen::RecoveryRequired);
        assert_eq!(observations, SIDECAR_REOBSERVATIONS + 1);
    }

    #[test]
    fn an_empty_schema_database_is_refused_by_observers_like_a_missing_one() {
        for wal in [false, true] {
            let root = generation_root();
            let path = generation_database(&root);
            if wal {
                let connection = Connection::open(&path).unwrap();
                connection
                    .execute_batch("PRAGMA journal_mode=WAL;")
                    .unwrap();
                drop(connection);
            } else {
                std::fs::write(&path, b"").unwrap();
            }
            let empty = open_error(EngagementStore::open_read_only(&path));
            let missing = open_error(EngagementStore::open_read_only(
                &root.join("state/orchestration/missing.sqlite3"),
            ));
            assert_eq!(empty.code, missing.code, "wal={wal}");
            assert!(
                !namespace_birth_path(&root.join("state")).exists(),
                "wal={wal}"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn a_failed_migration_restores_foreign_key_enforcement() {
        let root = generation_root();
        let path = legacy_database(&root, 1);
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "PRAGMA foreign_keys=OFF;
                     DROP TABLE delivery_receipts;
                     CREATE TABLE delivery_receipts(
                       sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                       task_id TEXT NOT NULL UNIQUE,
                       artifact_id TEXT NOT NULL,
                       delivered_at_ms INTEGER NOT NULL,
                       FOREIGN KEY(task_id) REFERENCES tasks(task_id),
                       FOREIGN KEY(artifact_id) REFERENCES artifacts(artifact_id)
                     ) STRICT;
                     INSERT INTO delivery_receipts(sequence,task_id,artifact_id,delivered_at_ms)
                     VALUES(1,'missing-task','missing-artifact',1);",
                )
                .unwrap();
            connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
        }
        // The end-to-end open fails on that dangling reference.
        let error = open_error(EngagementStore::open(&path));
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert_eq!(observed_generation(&path).as_deref(), Some("1"));

        // Foreign-key enforcement is connection state, and the failed migration
        // returns its connection to the pool. A reused connection must still
        // enforce it.
        {
            let mut lease =
                crate::storage_observation::CheckedConnection::open(&path, false).unwrap();
            assert!(migrate_v1_to_v2(&mut lease).is_err());
        }
        let lease = crate::storage_observation::CheckedConnection::open(&path, false).unwrap();
        let enforced: i64 = lease
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(enforced, 1, "a failed migration leaked foreign_keys=OFF");
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_workspace_maintenance_lease_excludes_every_writable_open() {
        let root = generation_root();
        let state = root.join("state");
        // The production layout a maintenance owner gates.
        std::fs::write(state.join("workspace.json"), b"{}").unwrap();
        for directory in ["runtime", "runtime/locks"] {
            std::fs::create_dir_all(state.join(directory)).unwrap();
            std::fs::set_permissions(
                state.join(directory),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let legacy = legacy_database(&root, 5);
        let before = std::fs::read(&legacy).unwrap();
        let lease = crate::ledger_generation::WorkspaceLease::acquire(&state, true).unwrap();
        assert!(
            crate::ledger_generation::WorkspaceLease::acquire(&state, true).is_err(),
            "an exclusive lease must exclude a second acquisition"
        );

        // A legacy database is neither migrated nor given a birth record.
        let error = open_error(EngagementStore::open(&legacy));
        assert_eq!(error.code, "CAPABILITY_UNSUPPORTED");
        assert_eq!(observed_generation(&legacy).as_deref(), Some("5"));
        assert_eq!(std::fs::read(&legacy).unwrap(), before);
        assert!(!namespace_birth_path(&state).exists());

        // A brand-new database is not even created.
        let fresh_root = generation_root();
        let fresh_state = fresh_root.join("state");
        std::fs::write(fresh_state.join("workspace.json"), b"{}").unwrap();
        for directory in ["runtime", "runtime/locks"] {
            std::fs::create_dir_all(fresh_state.join(directory)).unwrap();
            std::fs::set_permissions(
                fresh_state.join(directory),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let fresh = generation_database(&fresh_root);
        let fresh_lease =
            crate::ledger_generation::WorkspaceLease::acquire(&fresh_state, true).unwrap();
        let error = open_error(EngagementStore::open(&fresh));
        assert_eq!(error.code, "CAPABILITY_UNSUPPORTED");
        assert!(!fresh.exists());
        assert!(!namespace_birth_path(&fresh_state).exists());
        drop(fresh_lease);

        drop(lease);
        // Once the owner releases the lease the same opens succeed.
        drop(EngagementStore::open(&legacy).unwrap());
        assert!(require_delegation_namespace(&state).is_ok());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(fresh_root).unwrap();
    }

    #[test]
    fn concurrent_publication_of_one_birth_record_converges() {
        let birth_id = Uuid::now_v7();
        let root = generation_root();
        let path = generation_database(&root);
        let shared = path.clone();
        let first = std::thread::spawn(move || publish_birth_record(&shared, birth_id));
        let second = std::thread::spawn({
            let path = path.clone();
            move || publish_birth_record(&path, birth_id)
        });
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert_eq!(
            read_birth_record(&namespace_birth_path(&root.join("state"))).unwrap(),
            Some(birth_id)
        );

        // Two different ids can never both own one namespace.
        let other_root = generation_root();
        let other = generation_database(&other_root);
        let one = Uuid::now_v7();
        let two = Uuid::now_v7();
        let loser = std::thread::spawn({
            let path = other.clone();
            move || publish_birth_record(&path, two)
        });
        let winner = publish_birth_record(&other, one);
        let loser = loser.join().unwrap();
        assert!(winner.is_ok() ^ loser.is_ok());
        let recorded = read_birth_record(&namespace_birth_path(&other_root.join("state")))
            .unwrap()
            .unwrap();
        assert!(recorded == one || recorded == two);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(other_root).unwrap();
    }

    #[test]
    fn later_generation_steps_treat_generation_six_as_applied() {
        let root = generation_root();
        let path = generation_database(&root);
        drop(EngagementStore::open(&path).unwrap());
        let mut connection = Connection::open(&path).unwrap();
        // Each accepted legacy step is a no-op over the current generation.
        migrate_v2_to_v3(&mut connection).unwrap();
        migrate_v3_to_v4(&mut connection).unwrap();
        migrate_v4_to_v5(&mut connection).unwrap();
        assert_eq!(observed_generation(&path).as_deref(), Some("6"));
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_empty_database_is_bootstrapped_instead_of_refused() {
        for wal in [false, true] {
            let root = generation_root();
            let path = generation_database(&root);
            if wal {
                // A never-committed WAL database: an empty schema in WAL mode.
                let connection = Connection::open(&path).unwrap();
                connection
                    .execute_batch("PRAGMA journal_mode=WAL;")
                    .unwrap();
                drop(connection);
            } else {
                std::fs::write(&path, b"").unwrap();
            }
            let store = EngagementStore::open(&path).unwrap();
            assert_eq!(
                observed_generation(&path).as_deref(),
                Some(SCHEMA_VERSION.to_string().as_str())
            );
            assert!(database_birth_id(&store.connection).is_ok());
            assert!(namespace_birth_path(&root.join("state")).exists());
            drop(store);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn a_rollback_journal_is_observed_without_the_immutable_open() {
        let root = generation_root();
        let path = generation_database(&root);
        drop(EngagementStore::open(&path).unwrap());
        let _ = std::fs::remove_file(path.with_file_name("orchestration.sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_file_name("orchestration.sqlite3-shm"));
        std::fs::write(path.with_file_name("orchestration.sqlite3-journal"), b"").unwrap();
        assert_eq!(
            SidecarState::observe(&path).unwrap(),
            SidecarState::JournalOnly
        );
        // The probe chose the plain read-only open rather than the immutable one,
        // so a hot journal could fail closed instead of being ignored. A writable
        // recovery still converges on the same generation and namespace.
        drop(EngagementStore::open(&path).unwrap());
        assert_eq!(
            observed_generation(&path).as_deref(),
            Some(SCHEMA_VERSION.to_string().as_str())
        );
        for suffix in ["-wal", "-shm", "-journal"] {
            let _ =
                std::fs::remove_file(path.with_file_name(format!("orchestration.sqlite3{suffix}")));
        }
        assert_eq!(
            delegation_namespace(&root.join("state")).unwrap(),
            DelegationNamespace::Ready {
                birth_id: read_birth_record(&namespace_birth_path(&root.join("state")))
                    .unwrap()
                    .unwrap()
            }
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_immutable_generation_six_column_refuses_an_update() {
        let root = generation_root();
        let path = generation_database(&root);
        let store = EngagementStore::open(&path).unwrap();
        let connection = &store.connection;
        let digest = "a".repeat(64);
        connection
            .execute(
                "INSERT INTO dispatch_indexes(task_id,workspace_ref,run_ref,head_commit,
                   index_sha256,index_byte_length,file_count,baseline_bytes,created_at_ms)
                 VALUES('t','w',?1,'head',?2,4096,1,10,1)",
                params![Uuid::now_v7().to_string(), digest],
            )
            .unwrap();
        for update in [
            "UPDATE dispatch_indexes SET head_commit='other'",
            "UPDATE dispatch_indexes SET created_at_ms=2",
            "UPDATE dispatch_indexes SET baseline_bytes=11",
            "UPDATE dispatch_indexes SET file_count=2",
        ] {
            assert!(connection.execute(update, []).is_err(), "{update}");
        }
        connection
            .execute_batch(
                "INSERT INTO caller_scope_assignments(
                   assignment_id,binding_ref,podway_workspace_ref,podway_session_ref,
                   podway_attempt_ref,delegated_placement_ref,workspace_ref,
                   aggregate_scope_kind,aggregate_scope_ref,member_ref,max_rounds,
                   accounting_state,created_at_ms,updated_at_ms)
                 VALUES('a','b','w','s','p','d','workspace:fixture',
                        'external_engagement','engagement:1','member:1',4,
                        'counted_unresolved',1,1);",
            )
            .unwrap();
        for update in [
            "UPDATE caller_scope_assignments SET max_rounds=5",
            "UPDATE caller_scope_assignments SET workspace_ref='other'",
            "UPDATE caller_scope_assignments SET aggregate_scope_kind='orchestrated_session'",
            "UPDATE caller_scope_assignments SET aggregate_scope_ref='engagement:2'",
            "UPDATE caller_scope_assignments SET created_at_ms=2",
        ] {
            assert!(connection.execute(update, []).is_err(), "{update}");
        }
        // `accounting_state` and `updated_at_ms` stay mutable.
        connection
            .execute(
                "UPDATE caller_scope_assignments SET accounting_state='counted_accepted',
                   updated_at_ms=2 WHERE assignment_id='a'",
                [],
            )
            .unwrap();
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operation_receipt_and_refusal_fields_freeze_after_acceptance() {
        let root = generation_root();
        let path = generation_database(&root);
        let store = EngagementStore::open(&path).unwrap();
        let connection = &store.connection;
        let digest = format!("sha256:{}", "a".repeat(64));
        let insert = |key: &str| {
            connection
                .execute(
                    "INSERT INTO delegation_operations(
                       operation_ref,state,workspace_ref,controller_principal,controller_generation,
                       scope_kind,scope_ref,operation_kind,idempotency_key,
                       mutation_envelope_json,mutation_envelope_digest,accepted_subjects_json,
                       created_at_ms,updated_at_ms)
                     VALUES(?1,'prepared','workspace:fixture','principal:fixture',1,
                            'external_engagement','engagement:1','hire_external_specialist',?2,
                            '{}',?3,'{}',1,1)",
                    params![Uuid::now_v7().to_string(), key, digest],
                )
                .unwrap();
        };
        insert("k1");
        // A receipt may only appear with the accepted transition.
        assert!(
            connection
                .execute(
                    "UPDATE delegation_operations SET receipt_json='{}' WHERE idempotency_key='k1'",
                    [],
                )
                .is_err()
        );
        connection
            .execute(
                "UPDATE delegation_operations SET state='accepted',receipt_json='{}'
                  WHERE idempotency_key='k1'",
                [],
            )
            .unwrap();
        for update in [
            "UPDATE delegation_operations SET receipt_json='{\"a\":1}' WHERE idempotency_key='k1'",
            "UPDATE delegation_operations SET definitive_refusal='POLICY_DENIED' WHERE idempotency_key='k1'",
            "UPDATE delegation_operations SET refusal_revision=1 WHERE idempotency_key='k1'",
        ] {
            assert!(connection.execute(update, []).is_err(), "{update}");
        }
        // A refusal may only appear with the resolved-not-accepted transition.
        insert("k2");
        assert!(
            connection
                .execute(
                    "UPDATE delegation_operations SET definitive_refusal='POLICY_DENIED',
                       refusal_revision=1 WHERE idempotency_key='k2'",
                    [],
                )
                .is_err()
        );
        connection
            .execute(
                "UPDATE delegation_operations SET state='resolved_not_accepted',
                   definitive_refusal='POLICY_DENIED',refusal_revision=1
                 WHERE idempotency_key='k2'",
                [],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE delegation_operations SET definitive_refusal='OTHER'
                      WHERE idempotency_key='k2'",
                    [],
                )
                .is_err()
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fresh_database_is_born_at_generation_six_with_its_birth_record() {
        let root = generation_root();
        let path = generation_database(&root);
        let store = EngagementStore::open(&path).unwrap();
        assert_eq!(
            observed_generation(&path).as_deref(),
            Some(SCHEMA_VERSION.to_string().as_str())
        );
        for table in GENERATION_SIX_TABLES {
            assert!(table_exists(&store.connection, table), "missing {table}");
        }
        // The orchestration extension generation is unchanged.
        let extension: Option<String> = store
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key='orchestration_schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert!(extension.is_none() || extension.as_deref() == Some("3"));
        let birth_id = database_birth_id(&store.connection).unwrap();
        assert_eq!(
            std::fs::read_to_string(namespace_birth_path(&root.join("state"))).unwrap(),
            format!(
                "{{\"namespace_birth_id\":\"{birth_id}\",\"schema\":\"dolgorae.namespace-birth/v1\"}}\n"
            )
        );
        assert_eq!(
            require_delegation_namespace(&root.join("state")).unwrap(),
            birth_id
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generation_five_migrates_directly_and_never_backfills() {
        let root = generation_root();
        let path = legacy_database(&root, 5);
        let before_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        drop(EngagementStore::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            observed_generation(&path).as_deref(),
            Some(SCHEMA_VERSION.to_string().as_str())
        );
        assert!(database_birth_id(&connection).is_ok());
        for table in GENERATION_SIX_TABLES {
            assert!(table_exists(&connection, table), "missing {table}");
            let rows: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(rows, 0, "{table} was backfilled");
        }
        let canary: String = connection
            .query_row(
                "SELECT value FROM metadata WHERE key='legacy_canary'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(canary, "keep");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            before_mode
        );
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn generation_six_objects(connection: &Connection) -> Vec<String> {
        connection
            .prepare("SELECT name FROM sqlite_master ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Seed one historical row per accepted legacy table, as each shape allows.
    fn seed_legacy_rows(connection: &Connection, generation: u8) {
        connection
            .execute_batch(
                "INSERT INTO engagements(engagement_id,workspace_id,controller_sha256,
                   state,revision)
                 VALUES('engagement-1','workspace','aaaa','active',2);
                 INSERT INTO members(specialist_run_id,engagement_id,hire_operation_id,
                   configuration_sha256,state)
                 VALUES('run-1','engagement-1','hire-1','bbbb','active');
                 INSERT INTO tasks(task_id,engagement_id,specialist_run_id,
                   objective_sha256,state,result_sha256)
                 VALUES('task-1','engagement-1','run-1','cccc','delivered','dddd');
                 INSERT INTO operations(operation,idempotency_key,request_sha256,response_json)
                 VALUES('open','key-1','eeee','{}');
                 INSERT INTO events(engagement_id,kind,payload_sha256,previous_hash,
                   event_hash,created_at_ms)
                 VALUES('engagement-1','opened','ffff','gggg','hhhh',7);",
            )
            .unwrap();
        let (artifact, receipt) = if generation <= 2 {
            (
                "INSERT INTO artifacts(artifact_id,engagement_id,result_sha256,canonical_json)
                 VALUES('artifact-1','engagement-1','dddd','{}');",
                "INSERT INTO delivery_receipts(sequence,task_id,artifact_id,delivered_at_ms)
                 VALUES(3,'task-1','artifact-1',9);",
            )
        } else {
            (
                "INSERT INTO artifacts(artifact_id,engagement_id,task_id,specialist_run_id,
                   result_sha256,canonical_json)
                 VALUES('artifact-1','engagement-1','task-1','run-1','dddd','{}');",
                "INSERT INTO delivery_receipts(sequence,task_id,artifact_id,delivered_at_ms)
                 VALUES(3,'task-1','artifact-1',9);",
            )
        };
        connection.execute_batch(artifact).unwrap();
        connection.execute_batch(receipt).unwrap();
    }

    /// Every seeded historical value, read back after the whole chain.
    fn legacy_rows(connection: &Connection) -> Vec<String> {
        vec![
            connection
                .query_row(
                    "SELECT engagement_id||'|'||workspace_id||'|'||controller_sha256||'|'||
                            state||'|'||revision FROM engagements WHERE engagement_id='engagement-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT specialist_run_id||'|'||engagement_id||'|'||hire_operation_id||'|'||
                            configuration_sha256||'|'||state FROM members
                      WHERE specialist_run_id='run-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT task_id||'|'||engagement_id||'|'||specialist_run_id||'|'||
                            objective_sha256||'|'||state||'|'||result_sha256 FROM tasks
                      WHERE task_id='task-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT artifact_id||'|'||engagement_id||'|'||result_sha256||'|'||
                            canonical_json FROM artifacts WHERE artifact_id='artifact-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT sequence||'|'||task_id||'|'||artifact_id||'|'||delivered_at_ms
                       FROM delivery_receipts WHERE task_id='task-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT engagement_id||'|'||kind||'|'||payload_sha256||'|'||previous_hash||'|'||
                            event_hash||'|'||created_at_ms FROM events WHERE engagement_id='engagement-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT operation||'|'||idempotency_key||'|'||request_sha256||'|'||
                            response_json FROM operations WHERE idempotency_key='key-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        ]
    }

    #[test]
    fn every_historical_generation_migrates_to_generation_six() {
        for generation in 1..=5u8 {
            let root = generation_root();
            let path = legacy_database(&root, generation);
            let seeded = {
                let connection = Connection::open(&path).unwrap();
                seed_legacy_rows(&connection, generation);
                connection
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                    .unwrap();
                let rows = legacy_rows(&connection);
                drop(connection);
                rows
            };
            drop(
                EngagementStore::open(&path)
                    .unwrap_or_else(|error| panic!("generation {generation}: {}", error.code)),
            );
            assert_eq!(
                legacy_rows(&Connection::open(&path).unwrap()),
                seeded,
                "generation {generation} changed its historical rows"
            );
            let connection = Connection::open(&path).unwrap();
            assert_eq!(
                observed_generation(&path).as_deref(),
                Some(SCHEMA_VERSION.to_string().as_str()),
                "generation {generation} did not reach 6"
            );
            assert!(database_birth_id(&connection).is_ok());
            let canary: String = connection
                .query_row(
                    "SELECT value FROM metadata WHERE key='legacy_canary'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(canary, "keep", "generation {generation} lost history");
            assert_eq!(
                require_delegation_namespace(&root.join("state")).unwrap(),
                database_birth_id(&connection).unwrap()
            );
            drop(connection);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn interrupted_generation_six_migration_is_atomic() {
        // A failure before the commit leaves generation 5 complete and untouched.
        let root = generation_root();
        let path = legacy_database(&root, 5);
        let seeded = {
            let connection = Connection::open(&path).unwrap();
            seed_legacy_rows(&connection, 5);
            connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
            let rows = legacy_rows(&connection);
            drop(connection);
            rows
        };
        let error = open_error(EngagementStore::open_with_faults(
            &path,
            Arc::new(FailAt(EngagementBarrier::BeforeMigrationV6Commit)),
        ));
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert_eq!(observed_generation(&path).as_deref(), Some("5"));
        let connection = Connection::open(&path).unwrap();
        for table in GENERATION_SIX_TABLES {
            assert!(!table_exists(&connection, table), "leaked {table}");
        }
        assert!(
            connection
                .query_row(
                    "SELECT value FROM metadata WHERE key='namespace_birth_id'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .unwrap()
                .is_none()
        );
        assert!(!namespace_birth_path(&root.join("state")).exists());
        // The seeded historical rows are unchanged by the failed transaction.
        assert_eq!(legacy_rows(&connection), seeded);
        drop(connection);

        // A failure after the commit leaves a complete generation 6 with no
        // published birth record; the next open completes publication.
        let error = open_error(EngagementStore::open_with_faults(
            &path,
            Arc::new(FailAt(EngagementBarrier::AfterMigrationV6Commit)),
        ));
        assert_eq!(error.code, "INTERNAL_ERROR");
        assert_eq!(observed_generation(&path).as_deref(), Some("6"));
        assert!(!namespace_birth_path(&root.join("state")).exists());
        // Every generation-6 object, its indexes and triggers are present.
        let committed = Connection::open(&path).unwrap();
        let objects = generation_six_objects(&committed);
        for table in GENERATION_SIX_TABLES {
            assert!(objects.iter().any(|name| name == table), "missing {table}");
        }
        for object in [
            "delegation_key_scoped",
            "delegation_key_prealloc",
            "delegation_identity_fixed",
            "delegation_receipt_frozen",
            "delegation_state_transition",
            "assignment_identity_fixed",
            "caller_scope_budget_fixed",
            "accounting_transition",
            "caller_scope_namespace",
            "scope_membership_monotonic",
            "member_workload_monotonic",
            "release_admission_immutable",
            "release_terminal_frozen",
            "dispatch_indexes_by_run",
            "dispatch_index_fixed",
        ] {
            assert!(
                objects.iter().any(|name| name == object),
                "missing {object}"
            );
        }
        assert!(database_birth_id(&committed).is_ok());
        drop(committed);
        drop(EngagementStore::open(&path).unwrap());
        assert!(namespace_birth_path(&root.join("state")).exists());
        assert!(require_delegation_namespace(&root.join("state")).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_interrupted_fresh_creation_completes_on_the_next_open() {
        let root = generation_root();
        let path = generation_database(&root);
        let error = open_error(EngagementStore::open_with_faults(
            &path,
            Arc::new(FailAt(EngagementBarrier::BeforeMigrationV6Commit)),
        ));
        assert_eq!(error.code, "INTERNAL_ERROR");
        // The historical bootstrap committed, so generation 5 is valid but has no
        // birth id and no record.
        assert_eq!(observed_generation(&path).as_deref(), Some("5"));
        let connection = Connection::open(&path).unwrap();
        assert!(
            connection
                .query_row(
                    "SELECT value FROM metadata WHERE key='namespace_birth_id'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .unwrap()
                .is_none()
        );
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(connection);
        assert!(!namespace_birth_path(&root.join("state")).exists());
        // The next writable open migrates 5 -> 6 and writes both.
        let store = EngagementStore::open(&path).unwrap();
        assert_eq!(
            observed_generation(&path).as_deref(),
            Some(SCHEMA_VERSION.to_string().as_str())
        );
        assert!(database_birth_id(&store.connection).is_ok());
        assert!(namespace_birth_path(&root.join("state")).exists());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_generations_fail_closed_before_any_effect() {
        for (generation, extension) in [("7", None), ("6", Some("4"))] {
            let root = generation_root();
            let path = generation_database(&root);
            // A real generation-6 database, then the requested mutation.
            drop(EngagementStore::open(&path).unwrap());
            std::fs::remove_file(namespace_birth_path(&root.join("state"))).unwrap();
            let connection = Connection::open(&path).unwrap();
            connection
                .execute(
                    "UPDATE metadata SET value=?1 WHERE key='schema_version'",
                    [generation],
                )
                .unwrap();
            if let Some(extension) = extension {
                connection
                    .execute(
                        "INSERT OR REPLACE INTO metadata(key,value) \
                         VALUES('orchestration_schema_version',?1)",
                        [extension],
                    )
                    .unwrap();
            }
            let schema_before: Vec<String> = connection
                .prepare("SELECT name FROM sqlite_master ORDER BY name")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            // The fixture mutation must be durable before the sidecars are
            // cleared, or deleting the log would discard it.
            connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
            drop(connection);
            // The fixture itself may leave a WAL pair; the refusal below must not
            // create one from a clean start.
            let _ = std::fs::remove_file(path.with_file_name("orchestration.sqlite3-wal"));
            let _ = std::fs::remove_file(path.with_file_name("orchestration.sqlite3-shm"));
            std::fs::write(root.join("state/workspace.json"), b"{}").unwrap();
            // A writable open in this layout would take the exclusive maintenance
            // lease and create its lock; the gate must refuse before that.
            for directory in ["runtime", "runtime/locks"] {
                std::fs::create_dir_all(root.join("state").join(directory)).unwrap();
                std::fs::set_permissions(
                    root.join("state").join(directory),
                    std::fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
            let mode_before = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;

            let writable = open_error(EngagementStore::open(&path));
            assert_eq!(
                writable.code, "ORCHESTRATION_SCHEMA_UNSUPPORTED",
                "{}: {} {:?}",
                writable.code, writable.message, writable.details
            );
            let read_only = open_error(EngagementStore::open_read_only(&path));
            assert_eq!(read_only.code, "ORCHESTRATION_SCHEMA_UNSUPPORTED");
            let observer =
                crate::orchestration::OrchestrationStore::open_observer(&root.join("state"))
                    .err()
                    .expect("observer refuses");
            assert_eq!(observer.code, "ORCHESTRATION_SCHEMA_UNSUPPORTED");
            let orchestration = crate::orchestration::OrchestrationStore::open(&root.join("state"))
                .err()
                .expect("orchestration store refuses");
            assert_eq!(orchestration.code, "ORCHESTRATION_SCHEMA_UNSUPPORTED");

            // No sidecar, no maintenance lock, no permission change, no DDL.
            assert!(
                !root
                    .join("state/orchestration/orchestration.sqlite3-wal")
                    .exists()
            );
            assert!(
                !root
                    .join("state/orchestration/orchestration.sqlite3-shm")
                    .exists()
            );
            assert!(
                !root
                    .join("state/runtime/locks/ledger-maintenance.lock")
                    .exists()
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode_before
            );
            let connection =
                Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let schema_after: Vec<String> = connection
                .prepare("SELECT name FROM sqlite_master ORDER BY name")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(schema_before, schema_after);
            drop(connection);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn a_live_wal_pair_is_observed_without_touching_the_database() {
        let root = generation_root();
        let path = generation_database(&root);
        let store = EngagementStore::open(&path).unwrap();
        let birth_id = database_birth_id(&store.connection).unwrap();
        // A second live connection keeps a real WAL pair beside the database.
        let live = Connection::open(&path).unwrap();
        live.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        live.execute(
            "INSERT INTO metadata(key,value) VALUES('live_canary','1')",
            [],
        )
        .unwrap();
        assert_eq!(SidecarState::observe(&path).unwrap(), SidecarState::Both);
        let bytes_before = std::fs::read(&path).unwrap();
        let metadata_before: Vec<(String, String)> = live
            .prepare("SELECT key,value FROM metadata ORDER BY key")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let data_version_before: i64 = live
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .unwrap();
        let mode_before = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        // The read-only probe creates no file and changes no database state.
        let probe = admit_generation(&path).expect("probe succeeds");
        assert!(probe.is_some());
        assert_eq!(SidecarState::observe(&path).unwrap(), SidecarState::Both);
        assert_eq!(std::fs::read(&path).unwrap(), bytes_before);
        let metadata_after: Vec<(String, String)> = live
            .prepare("SELECT key,value FROM metadata ORDER BY key")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(metadata_after, metadata_before);
        assert_eq!(
            live.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            data_version_before
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            mode_before
        );
        assert_eq!(
            read_birth_record(&namespace_birth_path(&root.join("state"))).unwrap(),
            Some(birth_id)
        );
        drop(live);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn probe_refuses_unverified_shapes_and_inconsistent_sidecars() {
        // A database without `metadata` is not generation 1.
        let root = generation_root();
        let path = generation_database(&root);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE unrelated(key TEXT PRIMARY KEY) STRICT;")
            .unwrap();
        drop(connection);
        let error = open_error(EngagementStore::open(&path));
        assert_eq!(error.code, "ORCHESTRATION_SCHEMA_UNSUPPORTED");
        assert!(!namespace_birth_path(&root.join("state")).exists());

        // A WAL log without its shared memory is an inconsistent sidecar pair.
        std::fs::write(path.with_file_name("orchestration.sqlite3-wal"), b"").unwrap();
        let error = open_error(EngagementStore::open(&path));
        assert_eq!(error.code, "RECOVERY_REQUIRED");
        std::fs::remove_file(path.with_file_name("orchestration.sqlite3-wal")).unwrap();

        // Shared memory without a log is equally inconsistent.
        std::fs::write(path.with_file_name("orchestration.sqlite3-shm"), b"").unwrap();
        let error = open_error(EngagementStore::open_read_only(&path));
        assert_eq!(error.code, "RECOVERY_REQUIRED");
        std::fs::remove_file(path.with_file_name("orchestration.sqlite3-shm")).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delegation_namespace_reports_setup_recovery_and_absence() {
        // Neither record nor database.
        let root = generation_root();
        let state = root.join("state");
        assert_eq!(
            delegation_namespace(&state).unwrap(),
            DelegationNamespace::NotSetUp
        );
        assert_eq!(
            require_delegation_namespace(&state).unwrap_err().code,
            "DELEGATION_NAMESPACE_UNSET"
        );

        // A birth record without its database can never be authoritative.
        let birth = format!(
            "{{\"namespace_birth_id\":\"{}\",\"schema\":\"dolgorae.namespace-birth/v1\"}}\n",
            Uuid::now_v7()
        );
        std::fs::write(namespace_birth_path(&state), birth.as_bytes()).unwrap();
        assert_eq!(
            delegation_namespace(&state).unwrap(),
            DelegationNamespace::RecoveryRequired
        );
        assert_eq!(
            require_delegation_namespace(&state).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        std::fs::remove_file(namespace_birth_path(&state)).unwrap();

        // A completed migration sets the namespace up.
        let path = generation_database(&root);
        drop(EngagementStore::open(&path).unwrap());
        let ready = delegation_namespace(&state).unwrap();
        assert!(ready.is_ready());

        // A replaced birth record is a recovery condition, never a takeover.
        std::fs::write(
            namespace_birth_path(&state),
            format!(
                "{{\"namespace_birth_id\":\"{}\",\"schema\":\"dolgorae.namespace-birth/v1\"}}\n",
                Uuid::now_v7()
            ),
        )
        .unwrap();
        assert_eq!(
            delegation_namespace(&state).unwrap(),
            DelegationNamespace::RecoveryRequired
        );
        assert_eq!(
            ensure_delegation_namespace(&state).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_birth_record_is_republished_from_the_database() {
        let root = generation_root();
        let state = root.join("state");
        let path = generation_database(&root);
        let store = EngagementStore::open(&path).unwrap();
        let birth_id = database_birth_id(&store.connection).unwrap();
        drop(store);
        std::fs::remove_file(namespace_birth_path(&state)).unwrap();
        // Read-only observation reports the database's namespace as set up.
        assert_eq!(
            delegation_namespace(&state).unwrap(),
            DelegationNamespace::Ready { birth_id }
        );
        assert!(!namespace_birth_path(&state).exists());
        // The next writable open republishes the record.
        drop(EngagementStore::open(&path).unwrap());
        assert_eq!(
            read_birth_record(&namespace_birth_path(&state)).unwrap(),
            Some(birth_id)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_invalid_operation_state_combination_is_refused() {
        let root = generation_root();
        let path = generation_database(&root);
        let store = EngagementStore::open(&path).unwrap();
        let insert = |state: &str,
                      receipt: Option<bool>,
                      refusal: Option<bool>,
                      refusal_revision: Option<bool>,
                      key: &str| {
            let literal = |present: Option<bool>| match present {
                Some(true) => "1".to_owned(),
                _ => "NULL".to_owned(),
            };
            let sql = format!(
                "INSERT INTO delegation_operations(
                   operation_ref,state,workspace_ref,controller_principal,controller_generation,
                   scope_kind,scope_ref,operation_kind,idempotency_key,
                   mutation_envelope_json,mutation_envelope_digest,accepted_subjects_json,
                   receipt_json,definitive_refusal,refusal_revision,created_at_ms,updated_at_ms)
                 VALUES(?1,?2,'workspace:fixture','principal:fixture',1,
                        'external_engagement','engagement:1','hire_external_specialist',?3,
                        '{{}}',?4,'{{}}',{},{},{},1,1)",
                literal(receipt),
                literal(refusal),
                literal(refusal_revision)
            );
            store
                .connection
                .execute(
                    &sql,
                    params![
                        Uuid::now_v7().to_string(),
                        state,
                        key,
                        format!("sha256:{}", "a".repeat(64))
                    ],
                )
                .is_ok()
        };
        // Valid rows for each accepted state.
        assert!(insert("prepared", None, None, None, "v-prepared"));
        assert!(insert("unresolved", None, None, None, "v-unresolved"));
        assert!(insert("accepted", Some(true), None, None, "v-accepted"));
        assert!(insert(
            "resolved_not_accepted",
            None,
            Some(true),
            Some(true),
            "v-refused"
        ));
        // Accepted without a receipt.
        assert!(!insert(
            "accepted",
            None,
            None,
            None,
            "i-accepted-no-receipt"
        ));
        // Prepared and unresolved with a receipt, a refusal or a refusal revision.
        for state in ["prepared", "unresolved"] {
            assert!(!insert(
                state,
                Some(true),
                None,
                None,
                &format!("i-{state}-receipt")
            ));
            assert!(!insert(
                state,
                None,
                Some(true),
                None,
                &format!("i-{state}-refusal")
            ));
            assert!(!insert(
                state,
                None,
                None,
                Some(true),
                &format!("i-{state}-revision")
            ));
        }
        // Resolved without either refusal field.
        assert!(!insert(
            "resolved_not_accepted",
            None,
            None,
            Some(true),
            "i-refused-no-code"
        ));
        assert!(!insert(
            "resolved_not_accepted",
            None,
            Some(true),
            None,
            "i-refused-no-revision"
        ));
        // A receipt together with a refusal.
        assert!(!insert(
            "resolved_not_accepted",
            Some(true),
            Some(true),
            Some(true),
            "i-both"
        ));
        // A half-null scope is refused on otherwise valid rows.
        for (kind, reference) in [
            (Some("external_engagement"), None),
            (None, Some("engagement:1")),
        ] {
            let kind = kind.map_or("NULL".to_owned(), |value| format!("'{value}'"));
            let reference = reference.map_or("NULL".to_owned(), |value| format!("'{value}'"));
            let sql = format!(
                "INSERT INTO delegation_operations(
                   operation_ref,state,workspace_ref,controller_principal,controller_generation,
                   scope_kind,scope_ref,operation_kind,idempotency_key,
                   mutation_envelope_json,mutation_envelope_digest,accepted_subjects_json,
                   created_at_ms,updated_at_ms)
                 VALUES(?1,'prepared','workspace:fixture','principal:fixture',1,
                        {kind},{reference},'hire_external_specialist',?2,
                        '{{}}',?3,'{{}}',1,1)"
            );
            assert!(
                store
                    .connection
                    .execute(
                        &sql,
                        params![
                            Uuid::now_v7().to_string(),
                            format!("i-half-{kind}"),
                            format!("sha256:{}", "a".repeat(64))
                        ],
                    )
                    .is_err()
            );
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delegation_operation_states_and_partial_key_indexes_hold() {
        let root = generation_root();
        let path = generation_database(&root);
        let store = EngagementStore::open(&path).unwrap();
        let connection = &store.connection;
        let insert = |state: &str, scope: Option<(&str, &str)>, key: &str| -> bool {
            let (scope_kind, scope_ref) = match scope {
                Some((kind, reference)) => (kind.to_owned(), reference.to_owned()),
                None => (String::new(), String::new()),
            };
            let receipt = if state == "accepted" { "'{}'" } else { "NULL" };
            let refusal = if state == "resolved_not_accepted" {
                "'POLICY_DENIED'"
            } else {
                "NULL"
            };
            let refusal_revision = if state == "resolved_not_accepted" {
                "1"
            } else {
                "NULL"
            };
            let scope_kind = if scope_kind.is_empty() {
                "NULL".to_owned()
            } else {
                format!("'{scope_kind}'")
            };
            let scope_ref = if scope_ref.is_empty() {
                "NULL".to_owned()
            } else {
                format!("'{scope_ref}'")
            };
            connection
                .execute(
                    &format!(
                        "INSERT INTO delegation_operations(
                           operation_ref,schema_version,state,workspace_ref,controller_principal,
                           controller_generation,scope_kind,scope_ref,operation_kind,
                           idempotency_key,mutation_envelope_json,mutation_envelope_digest,
                           accepted_subjects_json,receipt_json,definitive_refusal,
                           refusal_revision,created_at_ms,updated_at_ms)
                         VALUES(?1,2,?2,'workspace:fixture','principal:fixture',1,
                                {scope_kind},{scope_ref},'install_provisioning',?3,
                                '{{}}',?4,'{{}}',{receipt},{refusal},{refusal_revision},1,1)"
                    ),
                    params![
                        Uuid::now_v7().to_string(),
                        state,
                        key,
                        format!("sha256:{}", "a".repeat(64))
                    ],
                )
                .is_ok()
        };
        // Every accepted state is insertable; no other state is.
        for state in [
            "prepared",
            "unresolved",
            "accepted",
            "resolved_not_accepted",
        ] {
            assert!(
                insert(state, Some(("external_engagement", "engagement:1")), state),
                "{state}"
            );
        }
        assert!(!insert(
            "settled",
            Some(("external_engagement", "engagement:1")),
            "bad"
        ));
        // A half-null scope is refused by the paired-scope CHECK.
        assert!(
            connection
                .execute(
                    "INSERT INTO delegation_operations(
                       operation_ref,workspace_ref,controller_principal,controller_generation,
                       scope_kind,scope_ref,operation_kind,idempotency_key,
                       mutation_envelope_json,mutation_envelope_digest,
                       accepted_subjects_json,created_at_ms,updated_at_ms)
                     VALUES(?1,'workspace:fixture','principal:fixture',1,
                            'external_engagement',NULL,'install_provisioning','half',
                            '{}',?2,'{}',1,1)",
                    params![
                        Uuid::now_v7().to_string(),
                        format!("sha256:{}", "a".repeat(64))
                    ],
                )
                .is_err()
        );
        // One live row per key. A definitive refusal resolves that row, and only
        // then may the same key be admitted again as a new operation.
        assert!(insert(
            "prepared",
            Some(("external_engagement", "engagement:2")),
            "retry-key"
        ));
        assert!(!insert(
            "unresolved",
            Some(("external_engagement", "engagement:2")),
            "retry-key"
        ));
        let resolved = connection
            .execute(
                "UPDATE delegation_operations
                    SET state='resolved_not_accepted',definitive_refusal='POLICY_DENIED',
                        refusal_revision=1
                  WHERE idempotency_key='retry-key'",
                [],
            )
            .unwrap();
        assert_eq!(resolved, 1);
        assert!(insert(
            "accepted",
            Some(("external_engagement", "engagement:2")),
            "retry-key"
        ));
        // The accepted row is terminal.
        assert!(
            connection
                .execute(
                    "UPDATE delegation_operations SET state='unresolved'
                      WHERE idempotency_key='retry-key' AND state='accepted'",
                    [],
                )
                .is_err()
        );
        // Pre-allocation keys collide on the null-scope partial index.
        assert!(insert("prepared", None, "install-key"));
        assert!(!insert("unresolved", None, "install-key"));
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn release_and_accounting_triggers_hold_their_invariants() {
        let root = generation_root();
        let path = generation_database(&root);
        let store = EngagementStore::open(&path).unwrap();
        let connection = &store.connection;
        connection
            .execute_batch(
                "INSERT INTO member_release_operations(
                   operation_ref,operation_kind,workspace_ref,scope_kind,scope_ref,member_ref,
                   run_ref,controller_principal,controller_generation,
                   admitted_membership_revision,owner_ref,progress,safe_failure_code,
                   required_action,terminal_membership_revision,settled_workload_revision,
                   created_at_ms,updated_at_ms)
                 VALUES('op','release_external_specialist','workspace:fixture',
                        'external_engagement','engagement:1','member:1','run:1',
                        'principal:fixture',1,4,NULL,'requested',NULL,'none',NULL,NULL,1,1);",
            )
            .unwrap();
        // Terminal state requires both frozen revisions and forbids a failure.
        assert!(
            connection
                .execute(
                    "UPDATE member_release_operations SET progress='terminal' \
                     WHERE operation_ref='op'",
                    [],
                )
                .is_err()
        );
        connection
            .execute(
                "UPDATE member_release_operations SET progress='terminal',
                   terminal_membership_revision=5,settled_workload_revision=2
                 WHERE operation_ref='op'",
                [],
            )
            .unwrap();
        // A settled release cannot be rewritten.
        assert!(
            connection
                .execute(
                    "UPDATE member_release_operations SET terminal_membership_revision=6 \
                     WHERE operation_ref='op'",
                    [],
                )
                .is_err()
        );
        // The initiating authority and admitted revision are immutable.
        assert!(
            connection
                .execute(
                    "UPDATE member_release_operations SET controller_generation=2 \
                     WHERE operation_ref='op'",
                    [],
                )
                .is_err()
        );

        connection
            .execute_batch(
                "INSERT INTO caller_scope_assignments(
                   assignment_id,binding_ref,podway_workspace_ref,podway_session_ref,
                   podway_attempt_ref,delegated_placement_ref,workspace_ref,
                   aggregate_scope_kind,aggregate_scope_ref,member_ref,max_rounds,
                   accounting_state,created_at_ms,updated_at_ms)
                 VALUES('a1','b','pw','ps','pa','dp','workspace:fixture',
                        'external_engagement','engagement:1','member:1',4,
                        'counted_unresolved',1,1);",
            )
            .unwrap();
        // `counted_accepted` is final for counting.
        connection
            .execute(
                "UPDATE caller_scope_assignments SET accounting_state='counted_accepted' \
                 WHERE assignment_id='a1'",
                [],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE caller_scope_assignments SET accounting_state='proved_nondispatch' \
                     WHERE assignment_id='a1'",
                    [],
                )
                .is_err()
        );
        // One immutable budget per caller scope.
        assert!(
            connection
                .execute(
                    "INSERT INTO caller_scope_assignments VALUES(
                       'a2','b','pw','ps','pa','dp','workspace:fixture',
                       'external_engagement','engagement:2','member:1',5,
                       'counted_unresolved',1,1)",
                    [],
                )
                .is_err()
        );
        // Revisions never decrease.
        connection
            .execute_batch(
                "INSERT INTO scope_membership_revisions VALUES('workspace:fixture',
                   'external_engagement','engagement:1',3);
                 INSERT INTO member_workload_revisions VALUES('workspace:fixture',
                   'external_engagement','engagement:1','member:1',2);",
            )
            .unwrap();
        assert!(
            connection
                .execute("UPDATE scope_membership_revisions SET revision=2", [],)
                .is_err()
        );
        assert!(
            connection
                .execute("UPDATE member_workload_revisions SET revision=1", [])
                .is_err()
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dispatch_index_bounds_and_the_run_aggregate_hold() {
        let root = generation_root();
        let path = generation_database(&root);
        let mut store = EngagementStore::open(&path).unwrap();
        let run_id = Uuid::now_v7();
        let workspace = "workspace:fixture";
        let transaction = store.transaction().unwrap();
        let digest = "a".repeat(64);
        record_dispatch_index(
            &transaction,
            DispatchIndexRecord {
                task_id: Uuid::now_v7(),
                workspace_ref: workspace,
                run_ref: run_id,
                head_commit: Some("0123456789abcdef"),
                index_sha256: &digest,
                index_byte_length: 4096,
                file_count: 12,
                baseline_bytes: 268_435_456,
            },
            7,
        )
        .unwrap();
        crate::storage_observation::commit(transaction).unwrap();
        let task: Uuid = store
            .connection
            .query_row("SELECT task_id FROM dispatch_indexes", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
            .parse()
            .unwrap();
        let (sha, length, files, baseline) =
            dispatch_index(&store.connection, task).unwrap().unwrap();
        assert_eq!(sha, digest);
        assert_eq!((length, files, baseline), (4096, 12, 268_435_456));

        // A second task would exceed the Run's 256 MiB aggregate.
        let transaction = store.transaction().unwrap();
        let error = record_dispatch_index(
            &transaction,
            DispatchIndexRecord {
                task_id: Uuid::now_v7(),
                workspace_ref: workspace,
                run_ref: run_id,
                head_commit: None,
                index_sha256: &digest,
                index_byte_length: 4096,
                file_count: 1,
                baseline_bytes: 1,
            },
            8,
        )
        .unwrap_err();
        assert_eq!(error.code, "STORAGE_CAPACITY_EXCEEDED");
        // A different Run starts its own aggregate.
        record_dispatch_index(
            &transaction,
            DispatchIndexRecord {
                task_id: Uuid::now_v7(),
                workspace_ref: workspace,
                run_ref: Uuid::now_v7(),
                head_commit: None,
                index_sha256: &digest,
                index_byte_length: 4096,
                file_count: 1,
                baseline_bytes: 1,
            },
            8,
        )
        .unwrap();
        // Per-row bounds refuse before dispatch.
        for (files, index_bytes, baseline) in [
            (10_001u64, 4096u64, 0u64),
            (1, 67_108_865, 0),
            (1, 1, 268_435_457),
        ] {
            let error = record_dispatch_index(
                &transaction,
                DispatchIndexRecord {
                    task_id: Uuid::now_v7(),
                    workspace_ref: workspace,
                    run_ref: Uuid::now_v7(),
                    head_commit: None,
                    index_sha256: &digest,
                    index_byte_length: index_bytes,
                    file_count: files,
                    baseline_bytes: baseline,
                },
                9,
            )
            .unwrap_err();
            assert_eq!(error.code, "STORAGE_CAPACITY_EXCEEDED");
        }
        crate::storage_observation::commit(transaction).unwrap();
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

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
            codex_version: "0.157.0".to_owned(),
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

    fn one_shot_input(root: &Path) -> OneShotOperationInput {
        OneShotOperationInput {
            request_ref: Uuid::now_v7(),
            workspace_id: "workspace".to_owned(),
            request: serde_json::json!({"version":3,"target":{"kind":"head"},"brief":"Check completion"}),
            profile: "reviewer".to_owned(),
            recovery_controller: owner_binding(),
            carrier_root: root.join("private-carriers"),
            temporary_server: true,
        }
    }

    #[test]
    fn one_shot_reservation_preserves_every_immutable_input_and_rejects_drift() {
        let (mut store, root) = store();
        let input = one_shot_input(&root);
        let (reserved, created) = store.reserve_one_shot(&input).unwrap();
        assert!(created);
        assert_eq!(
            store.reserve_one_shot(&input).unwrap(),
            (reserved.clone(), false)
        );
        let mut changes = Vec::new();
        let mut changed = input.clone();
        changed.workspace_id.push('x');
        changes.push(changed);
        let mut changed = input.clone();
        changed.request["brief"] = Value::String("different".to_owned());
        changes.push(changed);
        let mut changed = input.clone();
        changed.profile.push('x');
        changes.push(changed);
        let mut changed = input.clone();
        changed.recovery_controller.identity.instance_id.push('x');
        changes.push(changed);
        let mut changed = input.clone();
        changed.recovery_controller.capability_sha256 = "f".repeat(64);
        changes.push(changed);
        let mut changed = input.clone();
        changed.carrier_root.push("other");
        changes.push(changed);
        let mut changed = input.clone();
        changed.temporary_server = false;
        changes.push(changed);
        for changed in changes {
            assert_eq!(
                store.reserve_one_shot(&changed).unwrap_err().code,
                "IDEMPOTENCY_CONFLICT"
            );
            assert_eq!(
                store.one_shot(input.request_ref).unwrap(),
                Some(reserved.clone())
            );
        }
        let capture = Uuid::now_v7();
        store
            .reserve_one_shot_capture(input.request_ref, capture)
            .unwrap();
        store
            .reserve_one_shot_capture(input.request_ref, capture)
            .unwrap();
        assert_eq!(
            store
                .reserve_one_shot_capture(input.request_ref, Uuid::now_v7())
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        let terminal = OneShotTerminal::Failed {
            safe_error_code: "REVIEW_OUTPUT_INVALID".to_owned(),
        };
        store.finish_one_shot(input.request_ref, &terminal).unwrap();
        store.finish_one_shot(input.request_ref, &terminal).unwrap();
        assert_eq!(
            store
                .finish_one_shot(
                    input.request_ref,
                    &OneShotTerminal::Succeeded {
                        result: serde_json::json!({})
                    }
                )
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        assert!(
            store
                .finish_one_shot(
                    input.request_ref,
                    &OneShotTerminal::Failed {
                        safe_error_code: "private provider prose".to_owned()
                    }
                )
                .is_err()
        );
        assert_eq!(
            store.one_shot(input.request_ref).unwrap().unwrap().terminal,
            Some(terminal)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_shot_engagement_binding_is_atomic_across_open_commit_faults() {
        for barrier in [
            EngagementBarrier::BeforeOpenCommit,
            EngagementBarrier::AfterOpenCommit,
        ] {
            let (mut initial, root) = store();
            let input = one_shot_input(&root);
            initial.reserve_one_shot(&input).unwrap();
            drop(initial);
            let mut failed = store_with_fault(&root, barrier);
            assert!(
                failed
                    .open_one_shot_engagement(input.request_ref, &"a".repeat(64), "one-shot-open")
                    .is_err()
            );
            drop(failed);
            let mut restarted = reopen(&root);
            let observed = restarted.one_shot(input.request_ref).unwrap().unwrap();
            let count: i64 = restarted
                .connection
                .query_row("SELECT COUNT(*) FROM engagements", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                count,
                i64::from(barrier == EngagementBarrier::AfterOpenCommit)
            );
            assert_eq!(
                observed.engagement_id.is_some(),
                barrier == EngagementBarrier::AfterOpenCommit
            );
            let opened = restarted
                .open_one_shot_engagement(input.request_ref, &"a".repeat(64), "one-shot-open")
                .unwrap();
            assert_eq!(
                restarted
                    .one_shot(input.request_ref)
                    .unwrap()
                    .unwrap()
                    .engagement_id,
                Some(opened.engagement_id)
            );
            if let Some(id) = observed.engagement_id {
                assert_eq!(id, opened.engagement_id);
            }
            assert_eq!(
                restarted
                    .connection
                    .query_row("SELECT COUNT(*) FROM engagements", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert!(
                restarted
                    .open_one_shot_engagement(input.request_ref, &"a".repeat(64), "another-open")
                    .is_err()
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn one_shot_inspection_retains_diagnostic_and_artifact_without_delivering_or_replaying() {
        for invalid_output in [false, true] {
            let (mut store, root) = store();
            let input = one_shot_input(&root);
            store.reserve_one_shot(&input).unwrap();
            let opened = store
                .open_one_shot_engagement(input.request_ref, &"a".repeat(64), "open")
                .unwrap();
            let hired = store
                .hire_reviewer(
                    opened.engagement_id,
                    &reviewer_plan("review"),
                    "hire",
                    |_, _, _| RuntimeOutcome::Accepted,
                )
                .unwrap();
            let error =
                crate::review_output::invalid_output(Some("not-json"), None, "invalid_json", "");
            let task = store
                .assign(
                    opened.engagement_id,
                    hired.specialist_run_id,
                    &"b".repeat(64),
                    None,
                    "assign",
                    |_| {
                        if invalid_output {
                            TaskExecution {
                                outcome: RuntimeOutcome::Rejected,
                                value: None,
                                terminal_error: Some(error),
                            }
                        } else {
                            TaskExecution {
                                outcome: RuntimeOutcome::Accepted,
                                value: Some(serde_json::json!({"summary":"original result"})),
                                terminal_error: None,
                            }
                        }
                    },
                )
                .unwrap();
            let expected = task.terminal_error.as_ref().and_then(diagnostic);
            let before = store.snapshot(opened.engagement_id).unwrap();
            let event_count: i64 = store
                .connection
                .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
                .unwrap();
            drop(store);
            let observer =
                EngagementStore::open_read_only(&root.join("orchestration.sqlite3")).unwrap();
            for _ in 0..2 {
                let view = observer
                    .observe_one_shot(input.request_ref)
                    .unwrap()
                    .unwrap();
                assert_eq!(view.operation.input, input);
                assert_eq!(view.operation.engagement_id, Some(opened.engagement_id));
                assert_eq!(view.engagement.as_ref(), Some(&before));
                let observed_task = view.task.unwrap();
                assert_eq!(observed_task.task_id, task.task_id);
                if invalid_output {
                    assert_eq!(observed_task.diagnostic, expected.clone());
                    assert_eq!(
                        observed_task.safe_error_code.as_deref(),
                        Some("REVIEW_OUTPUT_INVALID")
                    );
                    assert!(view.artifact.is_none());
                } else {
                    assert_eq!(
                        view.artifact.unwrap().output,
                        serde_json::json!({"summary":"original result"})
                    );
                    assert!(observed_task.diagnostic.is_none());
                }
                assert_eq!(
                    observer
                        .connection
                        .query_row("SELECT COUNT(*) FROM delivery_receipts", [], |row| row
                            .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
                assert_eq!(
                    observer
                        .connection
                        .query_row("SELECT COUNT(*) FROM events", [], |row| row
                            .get::<_, i64>(0))
                        .unwrap(),
                    event_count
                );
            }
            assert!(
                observer
                    .connection
                    .execute("UPDATE metadata SET value='0'", [])
                    .is_err()
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn one_shot_observation_preserves_unknown_dispatch_without_reconciliation() {
        let (mut store, root) = store();
        let input = one_shot_input(&root);
        store.reserve_one_shot(&input).unwrap();
        let opened = store
            .open_one_shot_engagement(input.request_ref, &"a".repeat(64), "open")
            .unwrap();
        let hired = store
            .hire_reviewer(
                opened.engagement_id,
                &reviewer_plan("review"),
                "hire",
                |_, _, _| RuntimeOutcome::Accepted,
            )
            .unwrap();
        drop(store);
        let mut interrupted =
            store_with_fault(&root, EngagementBarrier::AfterTaskReservationCommit);
        assert!(
            interrupted
                .assign(
                    opened.engagement_id,
                    hired.specialist_run_id,
                    &"b".repeat(64),
                    None,
                    "assign",
                    |_| -> TaskExecution { panic!("fault must precede dispatch") },
                )
                .is_err()
        );
        drop(interrupted);
        let observer =
            EngagementStore::open_read_only(&root.join("orchestration.sqlite3")).unwrap();
        let first = observer
            .observe_one_shot(input.request_ref)
            .unwrap()
            .unwrap();
        assert_eq!(first.engagement.as_ref().unwrap().state, "executing");
        assert_eq!(first.task.as_ref().unwrap().state, "accepted");
        assert!(first.artifact.is_none());
        assert_eq!(
            observer.observe_one_shot(input.request_ref).unwrap(),
            Some(first)
        );
        assert!(
            observer
                .one_shot(input.request_ref)
                .unwrap()
                .unwrap()
                .terminal
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_shot_readers_preserve_the_original_json_digest() {
        for mode in ["reference", "retired_reference", "inline"] {
            let (mut store, root) = store();
            let input = one_shot_input(&root);
            store.reserve_one_shot(&input).unwrap();
            let opened = store
                .open_one_shot_engagement(input.request_ref, &"a".repeat(64), "open")
                .unwrap();
            let hired = store
                .hire_reviewer(
                    opened.engagement_id,
                    &reviewer_plan("review"),
                    "hire",
                    |_, _, _| RuntimeOutcome::Accepted,
                )
                .unwrap();
            store
                .assign(
                    opened.engagement_id,
                    hired.specialist_run_id,
                    &"b".repeat(64),
                    None,
                    "assign",
                    |_| TaskExecution {
                        outcome: RuntimeOutcome::Accepted,
                        value: Some(serde_json::json!({"summary":"alpha"})),
                        terminal_error: None,
                    },
                )
                .unwrap();
            let original = store
                .observe_one_shot(input.request_ref)
                .unwrap()
                .unwrap()
                .artifact
                .unwrap();
            let other = canonical_string(&serde_json::json!({"summary":"bravo"})).unwrap();
            if mode == "inline" {
                store
                    .connection
                    .execute(
                        "UPDATE artifacts SET canonical_json=?1 WHERE artifact_id=?2",
                        params![other, original.artifact_id.to_string()],
                    )
                    .unwrap();
            } else {
                crate::storage_observation::put_content(
                    &store.connection,
                    "external_artifact",
                    &Uuid::now_v7().to_string(),
                    "json",
                    other.as_bytes(),
                )
                .unwrap();
                store.connection.execute("UPDATE result_content_refs SET sha256=?1,retired_operation=?2 WHERE owner_kind='external_artifact' AND owner_id=?3",params![sha256_hex(other.as_bytes()), if mode == "retired_reference" {Some(Uuid::now_v7().to_string())} else {None}, original.artifact_id.to_string()]).unwrap();
            }
            assert!(
                store.collect_review(opened.engagement_id).is_err(),
                "{mode} collected altered JSON"
            );
            assert!(store.observe_one_shot(input.request_ref).is_err());
            assert_eq!(
                store
                    .connection
                    .query_row("SELECT COUNT(*) FROM delivery_receipts", [], |row| row
                        .get::<_, u64>(0))
                    .unwrap(),
                0
            );
            drop(store);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn one_shot_read_only_legacy_observation_does_not_migrate_or_create_storage() {
        let (mut store, root) = store();
        let existing = store
            .open_engagement("workspace", &"a".repeat(64), "legacy")
            .unwrap();
        drop(store);
        let database = root.join("orchestration.sqlite3");
        let fixture = Connection::open(&database).unwrap();
        fixture
            .execute_batch("DROP TABLE one_shot_operations;")
            .unwrap();
        downgrade_to_legacy_for_test(&database, &fixture, 4);
        drop(fixture);
        let observer = EngagementStore::open_read_only(&database).unwrap();
        assert!(observer.one_shot(Uuid::now_v7()).unwrap().is_none());
        assert_eq!(observer.snapshot(existing.engagement_id).unwrap(), existing);
        assert_eq!(
            observer
                .connection
                .query_row(
                    "SELECT value FROM metadata WHERE key='schema_version'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "4"
        );
        drop(observer);
        let migrated = EngagementStore::open(&database).unwrap();
        assert_eq!(migrated.snapshot(existing.engagement_id).unwrap(), existing);
        assert!(migrated.one_shot(Uuid::now_v7()).unwrap().is_none());
        assert_eq!(
            migrated
                .connection
                .query_row(
                    "SELECT value FROM metadata WHERE key='schema_version'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            SCHEMA_VERSION.to_string()
        );
        let missing = root.join("does-not-exist").join("orchestration.sqlite3");
        assert!(EngagementStore::open_read_only(&missing).is_err());
        assert!(!missing.parent().unwrap().exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_shot_receipt_integrity_and_foreign_authority_fail_closed() {
        let (mut store, root) = store();
        let input = one_shot_input(&root);
        store.reserve_one_shot(&input).unwrap();
        let opened = store
            .open_one_shot_engagement(input.request_ref, &"a".repeat(64), "open")
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE engagements SET authority_kind='external_v1' WHERE engagement_id=?1",
                [opened.engagement_id.to_string()],
            )
            .unwrap();
        assert!(store.observe_one_shot(input.request_ref).is_err());
        store
            .connection
            .execute(
                "UPDATE one_shot_operations SET receipt_sha256=?1",
                ["0".repeat(64)],
            )
            .unwrap();
        assert!(store.one_shot(input.request_ref).is_err());
        std::fs::remove_dir_all(root).unwrap();
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
    fn external_snapshot_requires_normalized_reference_and_preserves_legacy_and_retirement() {
        for mode in [
            "missing",
            "retired_missing",
            "retired",
            "legacy",
            "without_result",
            "null_binding",
            "malformed_binding",
            "missing_artifact",
            "missing_owner",
            "legacy_owners",
            "failed_without_result",
            "delivered",
            "delivered_null_binding",
            "retired_null_binding",
            "changed_content_reference",
            "changed_legacy_body",
            "changed_task_digest",
            "changed_artifact_digest",
            "retired_changed_reference",
        ] {
            let (mut store, root) = store();
            let opened = external_open(&mut store);
            let member = external_ready_member(&mut store, opened.engagement_id);
            let task = external_task_reservation(
                &mut store,
                opened.engagement_id,
                member.specialist_run_id,
            );
            if matches!(mode, "without_result" | "failed_without_result") {
                if mode == "failed_without_result" {
                    store
                        .finish_external_task(
                            opened.engagement_id,
                            task.task_id,
                            None,
                            "failed",
                            None,
                        )
                        .unwrap();
                }
                let snapshot = store
                    .external_task(opened.engagement_id, task.task_id)
                    .unwrap();
                assert_eq!(snapshot.task_id, task.task_id);
                assert!(snapshot.result.is_none());
                assert!(snapshot.result_artifact_ref.is_none());
            } else {
                let value = serde_json::json!({"summary":"retained external identity"});
                let mut completed = store
                    .finish_external_task(
                        opened.engagement_id,
                        task.task_id,
                        Some(&value),
                        "completed_not_delivered",
                        None,
                    )
                    .unwrap();
                let artifact = completed.result_artifact_ref.unwrap();
                if mode.starts_with("delivered") {
                    store
                        .collect_external_results(opened.engagement_id, 0, 8)
                        .unwrap();
                    completed = store
                        .external_task(opened.engagement_id, task.task_id)
                        .unwrap();
                    assert_eq!(completed.state, "delivered");
                }
                if mode.starts_with("retired") {
                    store.connection.execute("UPDATE result_content_refs SET retired_operation=?1 WHERE owner_kind='external_artifact' AND owner_id=?2",params![Uuid::now_v7().to_string(),artifact.to_string()]).unwrap();
                }
                if matches!(mode, "missing" | "retired_missing" | "legacy") {
                    store.connection.execute("DELETE FROM result_content_refs WHERE owner_kind='external_artifact' AND owner_id=?1",[artifact.to_string()]).unwrap();
                }
                if matches!(mode, "legacy" | "legacy_owners") {
                    store
                        .connection
                        .execute(
                            "UPDATE artifacts SET canonical_json=?1 WHERE artifact_id=?2",
                            params![canonical_string(&value).unwrap(), artifact.to_string()],
                        )
                        .unwrap();
                }
                match mode {
                    "null_binding" | "delivered_null_binding" | "retired_null_binding" => {
                        store
                            .connection
                            .execute(
                                "UPDATE tasks SET artifact_id=NULL WHERE task_id=?1",
                                [task.task_id.to_string()],
                            )
                            .unwrap();
                    }
                    "malformed_binding" => {
                        store
                            .connection
                            .execute(
                                "UPDATE tasks SET artifact_id='malformed' WHERE task_id=?1",
                                [task.task_id.to_string()],
                            )
                            .unwrap();
                    }
                    "missing_artifact" => {
                        store
                            .connection
                            .execute(
                                "DELETE FROM artifacts WHERE artifact_id=?1",
                                [artifact.to_string()],
                            )
                            .unwrap();
                    }
                    "missing_owner" => {
                        store.connection.execute("UPDATE artifacts SET task_id=NULL,specialist_run_id=NULL WHERE artifact_id=?1", [artifact.to_string()]).unwrap();
                    }
                    "legacy_owners" => {
                        store
                            .connection
                            .execute(
                                "UPDATE tasks SET request_json=NULL WHERE task_id=?1",
                                [task.task_id.to_string()],
                            )
                            .unwrap();
                        store.connection.execute("UPDATE artifacts SET task_id=NULL,specialist_run_id=NULL WHERE artifact_id=?1", [artifact.to_string()]).unwrap();
                    }
                    "changed_content_reference" | "retired_changed_reference" => {
                        let original_reference = crate::storage_observation::content_reference(
                            &store.connection,
                            "external_artifact",
                            &artifact.to_string(),
                            "json",
                        )
                        .unwrap()
                        .unwrap();
                        let other = canonical_string(
                            &serde_json::json!({"summary":"replaced external identity"}),
                        )
                        .unwrap();
                        assert_eq!(other.len(), canonical_string(&value).unwrap().len());
                        crate::storage_observation::put_content(
                            &store.connection,
                            "external_artifact",
                            &Uuid::now_v7().to_string(),
                            "json",
                            other.as_bytes(),
                        )
                        .unwrap();
                        store.connection.execute("UPDATE result_content_refs SET sha256=?1 WHERE owner_kind='external_artifact' AND owner_id=?2 AND representation='json'",params![sha256_hex(other.as_bytes()),artifact.to_string()]).unwrap();
                        if mode == "changed_content_reference" {
                            assert_eq!(
                                crate::storage_observation::read_referenced_content(
                                    &store.connection,
                                    &original_reference
                                )
                                .unwrap(),
                                canonical_string(&value).unwrap().into_bytes()
                            );
                        }
                    }
                    "changed_legacy_body" => {
                        let other = canonical_string(
                            &serde_json::json!({"summary":"replaced external identity"}),
                        )
                        .unwrap();
                        store
                            .connection
                            .execute(
                                "UPDATE artifacts SET canonical_json=?1 WHERE artifact_id=?2",
                                params![other, artifact.to_string()],
                            )
                            .unwrap();
                    }
                    "changed_task_digest" => {
                        store
                            .connection
                            .execute(
                                "UPDATE tasks SET result_sha256=?1 WHERE task_id=?2",
                                params!["0".repeat(64), task.task_id.to_string()],
                            )
                            .unwrap();
                    }
                    "changed_artifact_digest" => {
                        store
                            .connection
                            .execute(
                                "UPDATE artifacts SET result_sha256=?1 WHERE artifact_id=?2",
                                params!["0".repeat(64), artifact.to_string()],
                            )
                            .unwrap();
                    }
                    _ => {}
                }
                let observed = store.external_task(opened.engagement_id, task.task_id);
                if matches!(
                    mode,
                    "missing"
                        | "retired_missing"
                        | "null_binding"
                        | "malformed_binding"
                        | "missing_artifact"
                        | "missing_owner"
                        | "delivered_null_binding"
                        | "retired_null_binding"
                        | "changed_content_reference"
                        | "changed_legacy_body"
                        | "changed_task_digest"
                        | "changed_artifact_digest"
                        | "retired_changed_reference"
                ) {
                    assert_eq!(observed.unwrap_err().code, "INTERNAL_ERROR");
                } else {
                    let observed = observed.unwrap();
                    assert_eq!(observed.task_id, task.task_id);
                    assert_eq!(observed.result_artifact_ref, Some(artifact));
                    assert_eq!(observed.state, completed.state);
                    assert_eq!(
                        observed.result,
                        if mode == "retired" { None } else { Some(value) }
                    );
                }
            }
            drop(store);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn external_member_binding_uses_the_accepted_role_digest_for_its_configuration_generation() {
        let (mut store, root) = store();
        let opened = external_open(&mut store);
        let legacy = reviewer_plan("binding generation").agent_configuration;
        let instructions: crate::instructions::InstructionSnapshot = serde_json::from_str(
            include_str!("../docs/protocol/examples/workspace-execution-instructions.valid.json"),
        )
        .unwrap();
        let mut caller = legacy.clone();
        caller.schema_version = 3;
        caller.normalized_instructions = instructions.developer_prefix().unwrap();
        caller.instructions = crate::run::RunInstructionSnapshot::Caller(instructions.clone());
        let alias = "facade-reviewer-alias";
        for (key, configuration, role_digest) in [
            ("legacy", legacy, sha256_hex(alias.as_bytes())),
            ("caller", caller, instructions.role.sha256.clone()),
        ] {
            let member = store
                .reserve_external_hire(
                    opened.engagement_id,
                    alias,
                    &configuration,
                    "binding generation",
                    "read_only",
                    key,
                )
                .unwrap();
            store
                .finish_external_hire(&member, RuntimeOutcome::Accepted, key)
                .unwrap();
            let binding = AggregateBinding {
                aggregate_kind: AggregateKind::ExternalSpecialistEngagement,
                aggregate_id: opened.engagement_id,
                operation_id: member.hire_operation_id,
                member_kind: crate::run::AggregateMemberKind::Specialist,
                policy_sha256: None,
                role_reference: Some(alias.to_owned()),
                role_snapshot_sha256: Some(role_digest),
                agent_configuration_sha256: Some(
                    agent_configuration_digest(&configuration).unwrap(),
                ),
            };
            drop(store);
            store = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            store
                .validate_external_member_binding(
                    opened.engagement_id,
                    member.specialist_run_id,
                    &binding,
                )
                .unwrap();
            for field in ["role", "configuration", "operation", "reference"] {
                let mut wrong = binding.clone();
                match field {
                    "role" => wrong.role_snapshot_sha256 = Some("0".repeat(64)),
                    "configuration" => wrong.agent_configuration_sha256 = Some("0".repeat(64)),
                    "operation" => wrong.operation_id = Uuid::now_v7(),
                    "reference" => wrong.role_reference = Some("other".to_owned()),
                    _ => unreachable!(),
                }
                assert!(
                    store
                        .validate_external_member_binding(
                            opened.engagement_id,
                            member.specialist_run_id,
                            &wrong
                        )
                        .is_err()
                );
            }
            if configuration.schema_version == 3 {
                let mut old_digest = binding.clone();
                old_digest.role_snapshot_sha256 = Some(sha256_hex(alias.as_bytes()));
                assert!(
                    store
                        .validate_external_member_binding(
                            opened.engagement_id,
                            member.specialist_run_id,
                            &old_digest
                        )
                        .is_err()
                );
                let mut changed = configuration.clone();
                changed.model = "changed-stored-model".to_owned();
                store
                    .connection
                    .execute(
                        "UPDATE members SET configuration_json=?2 WHERE specialist_run_id=?1",
                        params![
                            member.specialist_run_id.to_string(),
                            serde_json::to_string(&changed).unwrap()
                        ],
                    )
                    .unwrap();
                let error = store
                    .validate_external_member_binding(
                        opened.engagement_id,
                        member.specialist_run_id,
                        &binding,
                    )
                    .unwrap_err();
                assert_eq!(
                    error.details["invariant"],
                    "stored member configuration digest does not match"
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
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
            let (reservation, first_attempt) = recovered
                .reserve_external_hire_with_provenance(
                    opened.engagement_id,
                    "reviewer",
                    &plan.agent_configuration,
                    "fault matrix",
                    "read_only",
                    "hire-fault",
                )
                .unwrap();
            assert_eq!(
                first_attempt,
                barrier == EngagementBarrier::BeforeExternalHireReservationCommit
            );
            let (replayed, replay_is_first_attempt) = recovered
                .reserve_external_hire_with_provenance(
                    opened.engagement_id,
                    "reviewer",
                    &plan.agent_configuration,
                    "fault matrix",
                    "read_only",
                    "hire-fault",
                )
                .unwrap();
            assert_eq!(replayed, reservation);
            assert!(!replay_is_first_attempt);
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
                 "#,
            )
            .unwrap();
        downgrade_to_legacy_for_test(&database, &connection, 1);
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
        // The accepted migration chain now ends at the current generation.
        assert_eq!(version, SCHEMA_VERSION.to_string());
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
                "migrated {table} schema diverges from a fresh v4 store"
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
                 ",
            )
            .unwrap();
        downgrade_to_legacy_for_test(&database, &connection, 1);
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
    fn reviewer_members_keep_their_durable_one_shot_or_reusable_authority() {
        let (mut store, root) = store();
        let one_shot = store
            .open_engagement("workspace", &"a".repeat(64), "one-shot")
            .unwrap();
        let plan = reviewer_plan("authority separation");
        let hired = store
            .hire_reviewer(
                one_shot.engagement_id,
                &plan,
                "one-shot-hire",
                |reservation, _, _| {
                    assert_eq!(reservation.engagement_id, one_shot.engagement_id);
                    RuntimeOutcome::Accepted
                },
            )
            .unwrap();
        assert!(
            !store
                .is_reusable_member(one_shot.engagement_id, hired.specialist_run_id)
                .unwrap()
        );
        let columns: (Option<String>, Option<String>) = store.connection.query_row(
            "SELECT configuration_json,requested_access FROM members WHERE specialist_run_id=?1",
            [hired.specialist_run_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(columns, (None, None));
        let reusable = external_open(&mut store);
        let member = external_ready_member(&mut store, reusable.engagement_id);
        assert!(
            store
                .is_reusable_member(reusable.engagement_id, member.specialist_run_id)
                .unwrap()
        );
        assert_eq!(
            store
                .external_member_configuration(reusable.engagement_id, member.specialist_run_id)
                .unwrap()
                .1,
            "read_only"
        );
        for (engagement_id, run_id) in [
            (one_shot.engagement_id, member.specialist_run_id),
            (reusable.engagement_id, hired.specialist_run_id),
            (Uuid::now_v7(), member.specialist_run_id),
        ] {
            assert_eq!(
                store
                    .is_reusable_member(engagement_id, run_id)
                    .unwrap_err()
                    .code,
                "SPECIALIST_NOT_MEMBER"
            );
        }
        store
            .connection
            .execute(
                "UPDATE members SET configuration_json=NULL WHERE specialist_run_id=?1",
                [member.specialist_run_id.to_string()],
            )
            .unwrap();
        assert!(
            store
                .is_reusable_member(reusable.engagement_id, member.specialist_run_id)
                .unwrap()
        );
        assert!(
            store
                .external_member_configuration(reusable.engagement_id, member.specialist_run_id)
                .is_err()
        );
        store
            .connection
            .execute(
                "UPDATE engagements SET authority_kind='unknown' WHERE engagement_id=?1",
                [reusable.engagement_id.to_string()],
            )
            .unwrap();
        assert_eq!(
            store
                .is_reusable_member(reusable.engagement_id, member.specialist_run_id)
                .unwrap_err()
                .code,
            "INTERNAL_ERROR"
        );
        assert!(
            !store
                .is_reusable_member(one_shot.engagement_id, hired.specialist_run_id)
                .unwrap()
        );
        let after: (Option<String>, Option<String>) = store.connection.query_row(
            "SELECT configuration_json,requested_access FROM members WHERE specialist_run_id=?1",
            [hired.specialist_run_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(after, columns);
        drop(store);
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
        let delivered_state: String = store
            .connection
            .query_row(
                "SELECT state FROM tasks WHERE task_id=?1",
                [reserved.task_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(delivered_state, "delivered");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v3_migration_preserves_legacy_failures_without_inventing_diagnostics() {
        let (mut store, root) = store();
        let opened = external_open(&mut store);
        let member = external_ready_member(&mut store, opened.engagement_id);
        let task =
            external_task_reservation(&mut store, opened.engagement_id, member.specialist_run_id);
        store
            .finish_external_task(
                opened.engagement_id,
                task.task_id,
                None,
                "failed",
                Some("REVIEW_OUTPUT_INVALID"),
            )
            .unwrap();
        store
            .connection
            .execute_batch("ALTER TABLE tasks DROP COLUMN diagnostic_json;")
            .unwrap();
        let database = root.join("orchestration.sqlite3");
        downgrade_to_legacy_for_test(&database, &store.connection, 3);
        drop(store);
        let mut migrated = EngagementStore::open(&database).unwrap();
        let historical = migrated
            .external_task(opened.engagement_id, task.task_id)
            .unwrap();
        assert_eq!(historical.state, "failed");
        assert_eq!(
            historical.safe_error_code.as_deref(),
            Some("REVIEW_OUTPUT_INVALID")
        );
        assert!(historical.diagnostic.is_none());
        assert!(
            serde_json::to_value(&historical)
                .unwrap()
                .get("diagnostic")
                .is_none()
        );
        assert!(
            migrated
                .collect_external_results(opened.engagement_id, 0, 16)
                .unwrap()
                .0
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_v4_unknown_task_retains_original_request_and_never_redispatches() {
        let (mut store, root) = store();
        let opened = store
            .open_engagement("workspace", &"a".repeat(64), "open-direct")
            .unwrap();
        let hired = store
            .hire_reviewer(
                opened.engagement_id,
                &reviewer_plan("review"),
                "hire-direct",
                |_, _, _| RuntimeOutcome::Accepted,
            )
            .unwrap();
        let accepted: Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/specialist-review-v4-request.valid.json"
        ))
        .unwrap();
        let mut task_value = accepted.clone();
        for field in ["schema", "operation", "target", "deadline_seconds", "role"] {
            task_value.as_object_mut().unwrap().remove(field);
        }
        let task: SpecialistTaskRequest = serde_json::from_value(task_value).unwrap();
        let assigned = store
            .assign_task_v3(
                opened.engagement_id,
                hired.specialist_run_id,
                &accepted,
                &task,
                "assign-direct",
                |_, _, _| Ok((RuntimeOutcome::Unknown, None)),
            )
            .unwrap();
        assert_eq!(assigned.state, "interrupted_unknown");
        drop(store);
        let mut reopened = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        let original: String = reopened
            .connection
            .query_row(
                "SELECT request_json FROM tasks WHERE task_id=?1",
                [assigned.task_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&original).unwrap(), accepted);
        let replay = reopened
            .assign_task_v3(
                opened.engagement_id,
                hired.specialist_run_id,
                &accepted,
                &task,
                "assign-direct",
                |_, _, _| panic!("an unknown original Turn must not be redispatched"),
            )
            .unwrap();
        assert_eq!(replay.task_id, assigned.task_id);
        assert_eq!(replay.specialist_run_id, hired.specialist_run_id);
        assert_eq!(replay.state, "interrupted_unknown");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_shot_invalid_output_commit_never_replays_a_turn_after_response_loss() {
        for barrier in [
            EngagementBarrier::BeforeTaskOutcomeCommit,
            EngagementBarrier::AfterTaskOutcomeCommit,
        ] {
            let (mut store, root) = store();
            let opened = store
                .open_engagement("workspace", &"a".repeat(64), "open-failure")
                .unwrap();
            let hired = store
                .hire_reviewer(
                    opened.engagement_id,
                    &reviewer_plan("review"),
                    "hire-failure",
                    |_, _, _| RuntimeOutcome::Accepted,
                )
                .unwrap();
            let task = SpecialistTaskRequest {
                purpose: "change".to_owned(),
                brief: "Review the candidate.".to_owned(),
                contexts: vec![],
                criteria: vec![],
                expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
            };
            let accepted =
                serde_json::json!({"schema":"dolgorae-specialist-review-request/v3", "task":task});
            drop(store);
            let mut faulted = store_with_fault(&root, barrier);
            let error = faulted
                .assign_task_v3(
                    opened.engagement_id,
                    hired.specialist_run_id,
                    &accepted,
                    &task,
                    "assign-failure",
                    |_, _, _| {
                        Err(crate::review_output::invalid_output(
                            Some("provider-secret"),
                            None,
                            "invalid_json",
                            "",
                        ))
                    },
                )
                .unwrap_err();
            assert_eq!(error.code, "INTERNAL_ERROR");
            drop(faulted);
            let mut restarted = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            let (state, diagnostic_json): (String, Option<String>) = restarted
                .connection
                .query_row(
                    "SELECT state,diagnostic_json FROM tasks WHERE engagement_id=?1",
                    [opened.engagement_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let replay = restarted.assign_task_v3(
                opened.engagement_id,
                hired.specialist_run_id,
                &accepted,
                &task,
                "assign-failure",
                |_, _, _| panic!("response loss must not dispatch another Turn"),
            );
            if barrier == EngagementBarrier::BeforeTaskOutcomeCommit {
                assert_eq!(state, "accepted");
                assert!(diagnostic_json.is_none());
                assert_eq!(replay.unwrap().state, "interrupted_unknown");
            } else {
                assert_eq!(state, "failed");
                let diagnostic_json = diagnostic_json.unwrap();
                assert!(!diagnostic_json.contains("provider-secret"));
                assert_eq!(
                    replay.unwrap_err().details["diagnostic"],
                    serde_json::from_str::<Value>(&diagnostic_json).unwrap()
                );
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn invalid_output_failure_and_diagnostic_commit_together_and_redeliver_after_restart() {
        for barrier in [
            EngagementBarrier::BeforeExternalTaskTerminalCommit,
            EngagementBarrier::AfterExternalTaskTerminalCommit,
        ] {
            let (mut store, root) = store();
            let opened = external_open(&mut store);
            let member = external_ready_member(&mut store, opened.engagement_id);
            let task = external_task_reservation(
                &mut store,
                opened.engagement_id,
                member.specialist_run_id,
            );
            let mut error = crate::review_output::invalid_output(
                Some("provider-secret"),
                None,
                "invalid_json",
                "",
            );
            attach_execution(
                &mut error,
                ReviewExecution {
                    run_id: Some(member.specialist_run_id),
                    task_id: Some(task.task_id),
                    model: Some("gpt-6-sol".to_owned()),
                    effort: Some("high".to_owned()),
                    turn_id: Some("turn-original".to_owned()),
                    ..Default::default()
                },
            );
            let expected = diagnostic(&error).unwrap();
            drop(store);
            let mut faulted = store_with_fault(&root, barrier);
            assert_eq!(
                faulted
                    .finish_external_task_with_diagnostic(
                        opened.engagement_id,
                        task.task_id,
                        None,
                        "failed",
                        Some("REVIEW_OUTPUT_INVALID"),
                        Some(&expected)
                    )
                    .unwrap_err()
                    .code,
                "INTERNAL_ERROR"
            );
            drop(faulted);
            let mut reopened = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            let observed = reopened
                .external_task(opened.engagement_id, task.task_id)
                .unwrap();
            if barrier == EngagementBarrier::BeforeExternalTaskTerminalCommit {
                assert_eq!(observed.state, task.state);
                assert!(observed.diagnostic.is_none());
                reopened
                    .finish_external_task_with_diagnostic(
                        opened.engagement_id,
                        task.task_id,
                        None,
                        "failed",
                        Some("REVIEW_OUTPUT_INVALID"),
                        Some(&expected),
                    )
                    .unwrap();
            } else {
                assert_eq!(observed.state, "failed");
                assert_eq!(observed.diagnostic.as_ref(), Some(&expected));
            }
            let (first, sequence) = reopened
                .collect_external_results(opened.engagement_id, 0, 16)
                .unwrap();
            assert_eq!(first.len(), 1);
            assert_eq!(first[0].state, "failed");
            assert_eq!(first[0].diagnostic.as_ref(), Some(&expected));
            assert!(first[0].result.is_none());
            assert!(first[0].result_artifact_ref.is_none());
            assert!(
                !serde_json::to_string(&first)
                    .unwrap()
                    .contains("provider-secret")
            );
            drop(reopened);
            let mut restarted = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
            let (replayed, replay_sequence) = restarted
                .collect_external_results(opened.engagement_id, 0, 16)
                .unwrap();
            assert_eq!(
                serde_json::to_value(replayed).unwrap(),
                serde_json::to_value(first).unwrap()
            );
            assert_eq!(sequence, replay_sequence);
            assert!(
                restarted
                    .collect_external_results(opened.engagement_id, sequence, 16)
                    .unwrap()
                    .0
                    .is_empty()
            );
            let artifact_count: i64 = restarted
                .connection
                .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
                .unwrap();
            assert_eq!(artifact_count, 0);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn diagnostic_rejects_incompatible_terminal_outcomes_without_mutation() {
        let (mut store, root) = store();
        let opened = external_open(&mut store);
        let member = external_ready_member(&mut store, opened.engagement_id);
        let task =
            external_task_reservation(&mut store, opened.engagement_id, member.specialist_run_id);
        let expected = diagnostic(&crate::review_output::invalid_output(
            Some("not-json"),
            None,
            "invalid_json",
            "",
        ))
        .unwrap();
        let before = serde_json::to_value(&task).unwrap();
        for (state, code) in [
            ("completed_not_delivered", "REVIEW_OUTPUT_INVALID"),
            ("failed", "OTHER_CODE"),
        ] {
            let error = store
                .finish_external_task_with_diagnostic(
                    opened.engagement_id,
                    task.task_id,
                    Some(&serde_json::json!({"summary":"must not be stored"})),
                    state,
                    Some(code),
                    Some(&expected),
                )
                .unwrap_err();
            assert_eq!(error.code, "INVALID_ARGUMENT");
            assert_eq!(error.details["argument"], "diagnostic");
            assert_eq!(
                serde_json::to_value(
                    store
                        .external_task(opened.engagement_id, task.task_id)
                        .unwrap()
                )
                .unwrap(),
                before
            );
            let artifact_count: i64 = store
                .connection
                .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
                .unwrap();
            assert_eq!(artifact_count, 0);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mixed_success_and_diagnostic_receipts_preserve_order_and_restart_replay() {
        use serde_json::json;

        let (mut store, root) = store();
        let opened = external_open(&mut store);
        let member = external_ready_member(&mut store, opened.engagement_id);
        let expected = diagnostic(&crate::review_output::invalid_output(
            Some("not-json"),
            None,
            "invalid_json",
            "",
        ))
        .unwrap();
        let mut task_ids = Vec::new();
        for number in 0..2 {
            let task = store
                .reserve_external_task(
                    opened.engagement_id,
                    member.specialist_run_id,
                    ExternalTaskRequest {
                        request_value: &json!({"task":number}),
                        objective: "exercise mixed collection",
                        external_request_ref: &json!({"kind":"mixed","id":number}),
                        execution_intent: "read_only",
                        deadline_seconds: 60,
                        idempotency_key: &format!("mixed-{number}"),
                    },
                )
                .unwrap();
            if number == 0 {
                store
                    .finish_external_task(
                        opened.engagement_id,
                        task.task_id,
                        Some(&json!({"summary":"valid result"})),
                        "completed_not_delivered",
                        None,
                    )
                    .unwrap();
            } else {
                store
                    .finish_external_task_with_diagnostic(
                        opened.engagement_id,
                        task.task_id,
                        None,
                        "failed",
                        Some("REVIEW_OUTPUT_INVALID"),
                        Some(&expected),
                    )
                    .unwrap();
            }
            store
                .connection
                .execute(
                    "UPDATE tasks SET created_at_ms=?1 WHERE task_id=?2",
                    params![number + 1, task.task_id.to_string()],
                )
                .unwrap();
            task_ids.push(task.task_id);
        }
        let (page, cursor) = store
            .collect_external_results(opened.engagement_id, 0, 100)
            .unwrap();
        assert_eq!(
            page.iter().map(|task| task.task_id).collect::<Vec<_>>(),
            task_ids
        );
        assert_eq!(page[0].state, "delivered");
        assert_eq!(page[0].result, Some(json!({"summary":"valid result"})));
        assert!(page[0].result_artifact_ref.is_some());
        assert!(page[0].diagnostic.is_none());
        assert_eq!(page[1].state, "failed");
        assert_eq!(page[1].diagnostic.as_ref(), Some(&expected));
        assert!(page[1].result.is_none());
        assert!(page[1].result_artifact_ref.is_none());
        assert_eq!(cursor, 2);
        drop(store);
        let mut restarted = EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        let (replayed, replay_cursor) = restarted
            .collect_external_results(opened.engagement_id, 0, 100)
            .unwrap();
        assert_eq!(
            serde_json::to_value(replayed).unwrap(),
            serde_json::to_value(page).unwrap()
        );
        assert_eq!(replay_cursor, cursor);
        assert!(
            restarted
                .collect_external_results(opened.engagement_id, cursor, 100)
                .unwrap()
                .0
                .is_empty()
        );
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
        let stored_diagnostic: String = store
            .connection
            .query_row(
                "SELECT diagnostic_json FROM tasks WHERE engagement_id=?1",
                [opened.engagement_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&stored_diagnostic).unwrap(),
            error.details["diagnostic"]
        );
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

    fn intent_namespace(kind: &str, key: &str, scope: Option<(&str, &str)>) -> LookupNamespace {
        LookupNamespace {
            controller_principal: format!("sha256:{}", "a".repeat(64)),
            controller_generation: 1,
            workspace_ref: "workspace".to_owned(),
            scope_kind: scope.map(|(kind, _)| kind.to_owned()),
            scope_ref: scope.map(|(_, reference)| reference.to_owned()),
            operation_kind: kind.to_owned(),
            idempotency_key: key.to_owned(),
        }
    }

    /// The closed mutation envelope and digest one intent row retains.
    fn intent_envelope_for(namespace: &LookupNamespace) -> (String, String) {
        let envelope = crate::operation::Envelope {
            schema: crate::operation::ENVELOPE_SCHEMA.to_owned(),
            request_schema: "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/install_provisioning_normalized_body".to_owned(),
            namespace: crate::operation::Namespace {
                controller_principal: namespace.controller_principal.clone(),
                controller_generation: namespace.controller_generation,
                workspace_ref: namespace.workspace_ref.clone(),
                scope_kind: namespace.scope_kind.clone(),
                scope_ref: namespace.scope_ref.clone(),
                operation_kind: namespace.operation_kind.clone(),
                idempotency_key: namespace.idempotency_key.clone(),
            },
            trusted_invocation_ref: None,
            request_body: crate::operation::BytesRef {
                byte_length: 2,
                sha256: format!("sha256:{}", "a".repeat(64)),
            },
        };
        let digest = crate::operation::envelope_digest(&envelope).unwrap();
        let json = crate::jcs::canonicalize(
            &crate::jcs::parse(&serde_json::to_string(&envelope).unwrap()).unwrap(),
        )
        .unwrap();
        (String::from_utf8(json).unwrap(), digest)
    }

    fn intent_receipt_for(
        namespace: &LookupNamespace,
        operation_ref: Uuid,
        digest: &str,
        revision: u64,
    ) -> OperationReceipt {
        let reference = Some(Uuid::now_v7().to_string());
        let subjects = match namespace.operation_kind.as_str() {
            "hire_external_specialist" => AcceptedSubjects {
                member_ref: reference,
                ..AcceptedSubjects::default()
            },
            "open_external_engagement" => AcceptedSubjects {
                engagement_ref: reference,
                ..AcceptedSubjects::default()
            },
            "install_provisioning" => AcceptedSubjects {
                provisioning_ref: reference,
                ..AcceptedSubjects::default()
            },
            _ => AcceptedSubjects {
                run_ref: reference,
                ..AcceptedSubjects::default()
            },
        };
        let mut receipt = OperationReceipt {
            schema: "dolgorae.operation-receipt/v1".to_owned(),
            operation_ref: operation_ref.to_string(),
            namespace: namespace.clone(),
            accepted_input_digest: digest.to_owned(),
            accepted_subjects: subjects,
            receipt_digest: String::new(),
            accepted_revision: revision,
            result_digest: None,
        };
        receipt.receipt_digest = receipt.compute_digest().unwrap();
        receipt
    }

    fn intent_admission<'a>(
        operation_ref: Uuid,
        namespace: &'a LookupNamespace,
        envelope_json: &'a str,
        digest: &'a str,
    ) -> IntentAdmission<'a> {
        IntentAdmission {
            operation_ref,
            namespace,
            mutation_envelope_json: envelope_json,
            mutation_envelope_digest: digest,
            expected_revision: None,
            now_ms: 1,
        }
    }

    /// Prepare one intent with its own closed envelope, returning its digest.
    fn prepare_intent(
        store: &mut EngagementStore,
        namespace: &LookupNamespace,
        operation_ref: Uuid,
    ) -> (IntentPreparation, String) {
        let (envelope, digest) = intent_envelope_for(namespace);
        let preparation = store
            .transact(|transaction| {
                intent_prepare(
                    transaction,
                    intent_admission(operation_ref, namespace, &envelope, &digest),
                )
            })
            .unwrap();
        (preparation, digest)
    }

    fn accept_intent(store: &mut EngagementStore, operation_ref: Uuid, receipt: &OperationReceipt) {
        store
            .transact(|transaction| intent_accept(transaction, operation_ref, receipt, 2))
            .unwrap();
    }

    #[test]
    fn intent_prepare_accept_and_lookup_hold_the_retained_receipt() {
        let (mut store, root) = store();
        let namespace = intent_namespace(
            "hire_external_specialist",
            "key-1",
            Some(("external_engagement", "engagement")),
        );
        let operation_ref = Uuid::now_v7();
        let (preparation, digest) = prepare_intent(&mut store, &namespace, operation_ref);
        assert!(matches!(preparation, IntentPreparation::Created));
        assert!(matches!(
            store.intent_lookup(&namespace).unwrap(),
            IntentLookup::Unresolved { operation_ref: retained, .. } if retained == operation_ref
        ));
        let receipt = intent_receipt_for(&namespace, operation_ref, &digest, 1);
        accept_intent(&mut store, operation_ref, &receipt);
        match store.intent_lookup(&namespace).unwrap() {
            IntentLookup::Accepted(retained) => assert_eq!(*retained, receipt),
            other => panic!("expected accepted, got {other:?}"),
        }
        // The exact key replays its retained receipt instead of a new intent.
        let (replay, _) = prepare_intent(&mut store, &namespace, operation_ref);
        match replay {
            IntentPreparation::Accepted(retained) => assert_eq!(*retained, receipt),
            _ => panic!("expected accepted replay"),
        }
        // The same key with another accepted input digest conflicts.
        let (other_envelope, other_digest) = intent_envelope_for(&namespace);
        let mut changed = other_envelope;
        changed = changed.replace(&digest, &format!("sha256:{}", "c".repeat(64)));
        assert_eq!(
            store
                .transact(|transaction| intent_prepare(
                    transaction,
                    intent_admission(
                        Uuid::now_v7(),
                        &namespace,
                        &changed,
                        &format!("sha256:{}", "c".repeat(64))
                    ),
                ))
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        assert!(other_digest.starts_with("sha256:"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_prepared_intent_reports_unresolved_and_records_its_uncertainty() {
        let (mut store, root) = store();
        let namespace = intent_namespace("open_external_engagement", "key-2", None);
        let operation_ref = Uuid::now_v7();
        let (_, digest) = prepare_intent(&mut store, &namespace, operation_ref);
        store
            .transact(|transaction| intent_mark_unresolved(transaction, operation_ref, 3))
            .unwrap();
        assert!(matches!(
            store.intent_lookup(&namespace).unwrap(),
            IntentLookup::Unresolved { operation_ref: retained, .. } if retained == operation_ref
        ));
        // An accepted intent never becomes unresolved again.
        let receipt = intent_receipt_for(&namespace, operation_ref, &digest, 0);
        accept_intent(&mut store, operation_ref, &receipt);
        assert_eq!(
            store
                .transact(|transaction| intent_mark_unresolved(transaction, operation_ref, 4))
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn case_a_refusal_is_terminal_and_reports_the_row_sequence() {
        let (mut store, root) = store();
        let namespace = intent_namespace("install_provisioning", "key-3", None);
        let operation_ref = Uuid::now_v7();
        let (_, digest) = prepare_intent(&mut store, &namespace, operation_ref);
        let revision = store
            .transact(|transaction| {
                intent_refuse_before_acceptance(transaction, operation_ref, "REVISION_CONFLICT", 5)
            })
            .unwrap();
        match store.intent_lookup(&namespace).unwrap() {
            IntentLookup::NotAccepted {
                operation_ref: retained,
                input_digest,
                refusal_code,
                durable_refusal_revision,
            } => {
                assert_eq!(retained, operation_ref);
                assert_eq!(input_digest, digest);
                assert_eq!(refusal_code, "REVISION_CONFLICT");
                assert_eq!(durable_refusal_revision, revision);
                assert!(revision > 0);
            }
            other => panic!("expected not_accepted, got {other:?}"),
        }
        // A refused intent is terminal for its operation reference.
        assert_eq!(
            store
                .transact(|transaction| {
                    intent_refuse_before_acceptance(transaction, operation_ref, "OTHER", 6)
                })
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_latest_refusal_is_selected_for_one_key() {
        let (mut store, root) = store();
        let namespace = intent_namespace("install_provisioning", "key-4", None);
        let first = Uuid::now_v7();
        prepare_intent(&mut store, &namespace, first);
        let first_revision = store
            .transact(|transaction| {
                intent_refuse_before_acceptance(transaction, first, "FIRST_REFUSAL", 7)
            })
            .unwrap();
        let second = Uuid::now_v7();
        prepare_intent(&mut store, &namespace, second);
        let second_revision = store
            .transact(|transaction| {
                intent_refuse_before_acceptance(transaction, second, "SECOND_REFUSAL", 8)
            })
            .unwrap();
        assert!(second_revision > first_revision);
        match store.intent_lookup(&namespace).unwrap() {
            IntentLookup::NotAccepted {
                operation_ref,
                refusal_code,
                durable_refusal_revision,
                ..
            } => {
                assert_eq!(operation_ref, second);
                assert_eq!(refusal_code, "SECOND_REFUSAL");
                assert_eq!(durable_refusal_revision, second_revision);
            }
            other => panic!("expected the latest refusal, got {other:?}"),
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_legacy_operations_key_is_refused_inside_admission_and_lookup() {
        let (mut store, root) = store();
        let namespace = intent_namespace(
            "hire_external_specialist",
            "legacy-key",
            Some(("external_engagement", "engagement")),
        );
        let (envelope, digest) = intent_envelope_for(&namespace);
        store
            .connection
            .execute(
                "INSERT INTO operations(scope_id,operation,idempotency_key,request_sha256,response_json)
                 VALUES('engagement','hire_external_specialist','legacy-key',?1,'{}')",
                [digest.clone()],
            )
            .unwrap();
        // Direct admission without a prior lookup is refused, not admitted.
        assert_eq!(
            store
                .transact(|transaction| intent_prepare(
                    transaction,
                    intent_admission(Uuid::now_v7(), &namespace, &envelope, &digest),
                ))
                .unwrap_err()
                .code,
            "RECOVERY_REQUIRED"
        );
        // Lookup never reports authoritative absence for a legacy-owned key.
        assert_eq!(
            store.intent_lookup(&namespace).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        let retained: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM delegation_operations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_corrupt_retained_intent_is_a_recovery_condition() {
        let (mut store, root) = store();
        let namespace = intent_namespace("install_provisioning", "key-5", None);
        let operation_ref = Uuid::now_v7();
        let (_, digest) = prepare_intent(&mut store, &namespace, operation_ref);
        let receipt = intent_receipt_for(&namespace, operation_ref, &digest, 0);
        accept_intent(&mut store, operation_ref, &receipt);
        store
            .connection
            .execute(
                "UPDATE delegation_operations SET accepted_subjects_json='{}' WHERE operation_ref=?1",
                [operation_ref.to_string()],
            )
            .unwrap();
        assert_eq!(
            store.intent_lookup(&namespace).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn absent_reports_the_durable_watermark_and_the_scope_revision_advances() {
        let (mut store, root) = store();
        let namespace = intent_namespace("install_provisioning", "key-6", None);
        assert!(matches!(
            store.intent_lookup(&namespace).unwrap(),
            IntentLookup::Absent {
                durable_watermark: 0
            }
        ));
        prepare_intent(&mut store, &namespace, Uuid::now_v7());
        match store.intent_lookup(&namespace).unwrap() {
            IntentLookup::Unresolved { .. } => {}
            other => panic!("expected unresolved, got {other:?}"),
        }
        let other_namespace = intent_namespace("install_provisioning", "key-7", None);
        match store.intent_lookup(&other_namespace).unwrap() {
            IntentLookup::Absent { durable_watermark } => assert!(durable_watermark >= 1),
            other => panic!("expected absent, got {other:?}"),
        }
        let first = store
            .transact(|transaction| {
                advance_scope_membership_revision(
                    transaction,
                    "workspace",
                    "external_engagement",
                    "engagement",
                )
            })
            .unwrap();
        let second = store
            .transact(|transaction| {
                advance_scope_membership_revision(
                    transaction,
                    "workspace",
                    "external_engagement",
                    "engagement",
                )
            })
            .unwrap();
        assert_eq!((first, second), (1, 2));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_refused_key_retries_only_with_its_latest_refused_input() {
        let (mut store, root) = store();
        let namespace = intent_namespace("install_provisioning", "refused-retry", None);
        let (envelope, digest) = intent_envelope_for(&namespace);
        let first = Uuid::now_v7();
        store
            .transact(|transaction| {
                intent_prepare(
                    transaction,
                    intent_admission(first, &namespace, &envelope, &digest),
                )
            })
            .unwrap();
        store
            .transact(|transaction| {
                intent_refuse_before_acceptance(transaction, first, "REVISION_CONFLICT", 9)
            })
            .unwrap();
        // A different accepted-input digest is a conflict and creates no row.
        let changed = format!("sha256:{}", "e".repeat(64));
        assert_eq!(
            store
                .transact(|transaction| {
                    intent_prepare(
                        transaction,
                        intent_admission(Uuid::now_v7(), &namespace, &envelope, &changed),
                    )
                })
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        let rows: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM delegation_operations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
        // The exact refused input is admitted as a new operation reference.
        let retry = Uuid::now_v7();
        match store
            .transact(|transaction| {
                intent_prepare(
                    transaction,
                    intent_admission(retry, &namespace, &envelope, &digest),
                )
            })
            .unwrap()
        {
            IntentPreparation::Created => {}
            _ => panic!("the exact refused input must be retryable"),
        }
        let receipt = intent_receipt_for(&namespace, retry, &digest, 0);
        accept_intent(&mut store, retry, &receipt);
        match store.intent_lookup(&namespace).unwrap() {
            IntentLookup::Accepted(retained) => {
                assert_eq!(retained.operation_ref, retry.to_string())
            }
            other => panic!("expected the retried acceptance, got {other:?}"),
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_corrupt_retained_envelope_or_receipt_is_a_recovery_condition() {
        let (mut store, root) = store();
        // An envelope that does not re-digest to the row's digest is refused.
        let namespace = intent_namespace("install_provisioning", "envelope-bogus", None);
        let (envelope, _) = intent_envelope_for(&namespace);
        let bogus = format!("sha256:{}", "b".repeat(64));
        store
            .transact(|transaction| {
                intent_prepare(
                    transaction,
                    intent_admission(Uuid::now_v7(), &namespace, &envelope, &bogus),
                )
            })
            .unwrap();
        assert_eq!(
            store.intent_lookup(&namespace).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        // A retained envelope that is not the closed envelope is refused too.
        let other = intent_namespace("open_external_engagement", "envelope-open", None);
        let (_, digest) = intent_envelope_for(&other);
        store
            .transact(|transaction| {
                intent_prepare(
                    transaction,
                    intent_admission(Uuid::now_v7(), &other, "{\"schema\":\"other\"}", &digest),
                )
            })
            .unwrap();
        assert_eq!(
            store.intent_lookup(&other).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        // A receipt whose retained digest does not verify is the same class.
        let receipt_namespace = intent_namespace("install_provisioning", "receipt", None);
        let operation_ref = Uuid::now_v7();
        let (_, receipt_digest) = prepare_intent(&mut store, &receipt_namespace, operation_ref);
        let mut receipt = intent_receipt_for(&receipt_namespace, operation_ref, &receipt_digest, 0);
        receipt.receipt_digest = format!("sha256:{}", "f".repeat(64));
        accept_intent(&mut store, operation_ref, &receipt);
        assert_eq!(
            store.intent_lookup(&receipt_namespace).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_envelope_with_duplicate_keys_or_a_corrupt_refusal_is_a_recovery_condition() {
        let (mut store, root) = store();
        // A duplicate object key is not the closed envelope: the JCS reader
        // refuses it before any shape or digest comparison.
        let duplicate = intent_namespace("install_provisioning", "duplicate-key", None);
        let (_, digest) = intent_envelope_for(&duplicate);
        store
            .transact(|transaction| {
                intent_prepare(
                    transaction,
                    intent_admission(
                        Uuid::now_v7(),
                        &duplicate,
                        "{\"schema\":\"a\",\"schema\":\"b\"}",
                        &digest,
                    ),
                )
            })
            .unwrap();
        assert_eq!(
            store.intent_lookup(&duplicate).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        // A refused row is retained evidence too: when its envelope does not
        // verify it can be neither not_accepted nor a re-entry basis.
        let corrupt = intent_namespace("install_provisioning", "corrupt-refusal", None);
        let (envelope, _) = intent_envelope_for(&corrupt);
        let wrong = format!("sha256:{}", "b".repeat(64));
        let refused = Uuid::now_v7();
        store
            .transact(|transaction| {
                intent_prepare(
                    transaction,
                    intent_admission(refused, &corrupt, &envelope, &wrong),
                )
            })
            .unwrap();
        store
            .transact(|transaction| {
                intent_refuse_before_acceptance(transaction, refused, "REVISION_CONFLICT", 11)
            })
            .unwrap();
        assert_eq!(
            store.intent_lookup(&corrupt).unwrap_err().code,
            "RECOVERY_REQUIRED"
        );
        // A refused key may not be re-admitted on a corrupt refusal either.
        assert_eq!(
            store
                .transact(|transaction| {
                    intent_prepare(
                        transaction,
                        intent_admission(Uuid::now_v7(), &corrupt, &envelope, &wrong),
                    )
                })
                .unwrap_err()
                .code,
            "RECOVERY_REQUIRED"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
