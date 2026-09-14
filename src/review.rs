//! One-shot read-only Specialist Review coordinator.

use crate::controller::{CredentialCarrier, binding_from_carrier, create_controller_credential};
use crate::domain::{ControllerKind, RunLifecycle};
use crate::engagement::{EngagementStore, RuntimeOutcome};
use crate::machine::{MachineError, new_uuid_v7};
use crate::run::RunStore;
use crate::semantic::{
    ReviewerStartContext, RunVerb, control_reviewer_run, prepare_reviewer, start_reviewer_run,
};
use crate::specialist::{
    ReviewerOutput, ReviewerOutputV3, validate_reviewer_output, validate_reviewer_output_v3,
};
use crate::task_request::{
    MAX_TASK_REQUEST_BYTES, SpecialistTaskRequest, TaskContext, TaskCriterion,
};
use crate::workspace::{SystemWorkspacePlatform, WorkspaceService};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const REVIEW_OBJECTIVE: &str = "Review the current working tree for concrete correctness, regression, concurrency, error-handling, security, test-coverage, and maintainability issues.";
const DEFAULT_REVIEW_DEADLINE_SECONDS: u64 = 600;
const MAX_REVIEW_REVISION_BYTES: usize = 1024;
const REVIEW_REQUEST_V3_SCHEMA: &str = "dolgorae-specialist-review-request/v3";
pub const REVIEW_FOCUS: [&str; 7] = [
    "correctness",
    "regressions",
    "concurrency",
    "error_handling",
    "security",
    "test_coverage",
    "maintainability",
];

static INTERRUPT_EPOCH: AtomicU64 = AtomicU64::new(0);
static INTERRUPT_HANDLER: OnceLock<Result<(), String>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub operation: String,
    pub scope: String,
    pub objective: String,
    pub focus: Vec<String>,
    pub expected_output: String,
    pub deadline_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTargetRequest {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequestV3 {
    pub schema: String,
    pub operation: String,
    pub target: ReviewTargetRequest,
    pub purpose: String,
    pub brief: String,
    pub contexts: Vec<TaskContext>,
    pub criteria: Vec<TaskCriterion>,
    pub expected_output: String,
    pub deadline_seconds: u64,
}

enum CheckedScopedOutput {
    V2(ReviewerOutput),
    V3(ReviewerOutputV3),
}

impl ReviewRequestV3 {
    pub fn validate(&self) -> Result<SpecialistTaskRequest, MachineError> {
        if self.schema != REVIEW_REQUEST_V3_SCHEMA || self.operation != "review_target" {
            return Err(MachineError::invalid_argument(
                "review_request",
                "request must use the checked v3 review schema and operation",
            ));
        }
        self.target.validate()?;
        if !(1..=3_600).contains(&self.deadline_seconds) {
            return Err(MachineError::invalid_argument(
                "deadline_seconds",
                "deadline must be between 1 and 3600 seconds",
            ));
        }
        let task = SpecialistTaskRequest {
            purpose: self.purpose.clone(),
            brief: self.brief.clone(),
            contexts: self.contexts.clone(),
            criteria: self.criteria.clone(),
            expected_output: self.expected_output.clone(),
        };
        task.validate_review()?;
        Ok(task)
    }
}

impl ReviewTargetRequest {
    pub fn validate(&self) -> Result<(), MachineError> {
        let known = ["workspace", "staged", "dirty", "head", "commit", "range"];
        if !known.contains(&self.kind.as_str()) {
            return Err(MachineError::invalid_argument(
                "--target-kind",
                "target kind must be workspace, staged, dirty, head, commit, or range",
            ));
        }
        if self
            .revision
            .as_ref()
            .is_some_and(|value| value.len() > MAX_REVIEW_REVISION_BYTES)
        {
            return Err(MachineError::invalid_argument(
                "--revision",
                "revision must be at most 1024 bytes",
            ));
        }
        match (self.kind.as_str(), self.revision.as_deref()) {
            ("workspace" | "staged" | "dirty" | "head", None) => Ok(()),
            ("commit", Some(value)) if !value.is_empty() => Ok(()),
            ("range", Some(value)) if exact_range(value) => Ok(()),
            ("workspace" | "staged" | "dirty" | "head", Some(_)) => Err(
                MachineError::invalid_argument("--revision", "this target kind rejects a revision"),
            ),
            ("commit" | "range", None) => Err(MachineError::invalid_argument(
                "--revision",
                "this target kind requires a revision",
            )),
            _ => Err(MachineError::invalid_argument(
                "--revision",
                "range requires one exact A..B or A...B expression",
            )),
        }
    }
}

fn exact_range(value: &str) -> bool {
    if value.contains("....") {
        return false;
    }
    let separator = if value.contains("...") { "..." } else { ".." };
    let Some((left, right)) = value.split_once(separator) else {
        return false;
    };
    !left.is_empty() && !right.is_empty() && !left.contains("..") && !right.contains("..")
}

impl ReviewRequest {
    #[must_use]
    pub fn fixed() -> Self {
        Self {
            operation: "review_working_tree".to_owned(),
            scope: "working_tree".to_owned(),
            objective: REVIEW_OBJECTIVE.to_owned(),
            focus: REVIEW_FOCUS.iter().map(ToString::to_string).collect(),
            expected_output: "structured_findings_v1".to_owned(),
            deadline_seconds: DEFAULT_REVIEW_DEADLINE_SECONDS,
        }
    }

    pub fn validate(&self) -> Result<(), MachineError> {
        if self.operation != "review_working_tree"
            || self.scope != "working_tree"
            || self.expected_output != "structured_findings_v1"
            || self.deadline_seconds == 0
            || self.deadline_seconds > 3_600
            || self.objective.is_empty()
            || self.objective.len() > 65_536
            || self.objective.chars().any(char::is_control)
            || self.focus.is_empty()
            || self.focus.len() > REVIEW_FOCUS.len()
            || self
                .focus
                .iter()
                .any(|value| !REVIEW_FOCUS.contains(&value.as_str()))
        {
            return Err(MachineError::invalid_argument(
                "review_request",
                "request does not match the checked working-tree review contract",
            ));
        }
        let mut unique = self.focus.clone();
        unique.sort();
        unique.dedup();
        if unique.len() != self.focus.len() {
            return Err(MachineError::invalid_argument(
                "focus",
                "review focus dimensions must be unique",
            ));
        }
        Ok(())
    }
}

struct EphemeralCarriers {
    root: PathBuf,
    aggregate: PathBuf,
    reviewer: PathBuf,
    settlement_owner: PathBuf,
    terminal_receipt: PathBuf,
}

struct InterruptWatcher {
    done: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl InterruptWatcher {
    fn start(interrupt_epoch: u64, state_root: PathBuf, controller: PathBuf, run_id: Uuid) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let watcher_done = Arc::clone(&done);
        let handle = thread::spawn(move || {
            while !watcher_done.load(Ordering::SeqCst) {
                if interrupted_since(interrupt_epoch)
                    && RunStore::new(SystemWorkspacePlatform, &state_root)
                        .load_state_projection(run_id)
                        .is_ok_and(|projection| {
                            matches!(
                                projection.lifecycle,
                                RunLifecycle::Running | RunLifecycle::WaitingInteraction
                            )
                        })
                {
                    let _ =
                        crate::semantic::interrupt_reviewer_run(&state_root, run_id, &controller);
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        Self {
            done,
            handle: Some(handle),
        }
    }

    fn stop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for InterruptWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

impl EphemeralCarriers {
    fn create(state_root: &Path) -> Result<Self, MachineError> {
        let orchestration = state_root.join("orchestration");
        fs::create_dir_all(&orchestration).map_err(internal)?;
        fs::set_permissions(&orchestration, fs::Permissions::from_mode(0o700)).map_err(internal)?;
        let root = orchestration.join(format!(".specialist-review-{}", new_uuid_v7()));
        fs::create_dir_all(&root).map_err(internal)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(internal)?;
        let aggregate = root.join("aggregate.json");
        let reviewer = root.join("reviewer.json");
        let settlement_owner = root.join("settlement-owner");
        let terminal_receipt = root.join("terminal-receipt.json");
        create_controller_credential(
            &aggregate,
            ControllerKind::WorkflowOrchestrator,
            format!("specialist-review-{}", new_uuid_v7()),
            None,
            None,
        )?;
        create_controller_credential(
            &reviewer,
            ControllerKind::Automation,
            format!("reviewer-{}", new_uuid_v7()),
            None,
            None,
        )?;
        Ok(Self {
            root,
            aggregate,
            reviewer,
            settlement_owner,
            terminal_receipt,
        })
    }
}

impl Drop for EphemeralCarriers {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.aggregate);
        let _ = fs::remove_file(&self.reviewer);
        let _ = fs::remove_file(&self.settlement_owner);
        let _ = fs::remove_file(&self.terminal_receipt);
        let _ = fs::remove_dir(&self.root);
    }
}

pub fn execute_cli(arguments: &[OsString]) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(arguments, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let profile = required(arguments, "--profile")?;
    if required(arguments, "--format")? != "json" {
        return Err(MachineError::invalid_argument(
            "--format",
            "the canonical specialist review format is json",
        ));
    }
    if arguments
        .iter()
        .any(|argument| argument == "--request-stdin")
    {
        if std::io::stdin().is_terminal() {
            return Err(MachineError::invalid_argument(
                "--request-stdin",
                "v3 review request requires non-TTY stdin",
            ));
        }
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((MAX_TASK_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                MachineError::invalid_argument("--request-stdin", "request read failed")
            })?;
        if bytes.len() > MAX_TASK_REQUEST_BYTES {
            return Err(MachineError::invalid_argument(
                "--request-stdin",
                "v3 review request exceeds 1048576 bytes",
            ));
        }
        let request: ReviewRequestV3 = serde_json::from_slice(&bytes).map_err(|_| {
            MachineError::invalid_argument(
                "--request-stdin",
                "stdin does not match the checked v3 review request",
            )
        })?;
        return execute_v3(workspace.as_deref(), &profile, &request, new_uuid_v7());
    }
    match (
        optional_value(arguments, "--scope")?,
        optional_value(arguments, "--target-kind")?,
    ) {
        (Some(scope), None) if scope == "working-tree" => execute(
            workspace.as_deref(),
            &profile,
            &ReviewRequest::fixed(),
            new_uuid_v7(),
        ),
        (None, Some(kind)) => {
            let target = ReviewTargetRequest {
                kind,
                revision: optional_value(arguments, "--revision")?,
            };
            let deadline_seconds = optional_value(arguments, "--deadline-seconds")?
                .map(|value| {
                    value.parse::<u64>().map_err(|_| {
                        MachineError::invalid_argument(
                            "--deadline-seconds",
                            "deadline must be an integer",
                        )
                    })
                })
                .transpose()?
                .unwrap_or(DEFAULT_REVIEW_DEADLINE_SECONDS);
            execute_scoped(
                workspace.as_deref(),
                &profile,
                &target,
                deadline_seconds,
                new_uuid_v7(),
            )
        }
        (Some(_), Some(_)) => Err(MachineError::invalid_argument(
            "argv",
            "--scope and --target-kind are mutually exclusive",
        )),
        _ => Err(MachineError::invalid_argument(
            "argv",
            "use --scope working-tree for v1 or --target-kind for v2",
        )),
    }
}

pub fn execute(
    workspace: Option<&Path>,
    profile: &str,
    request: &ReviewRequest,
    request_ref: Uuid,
) -> Result<Value, MachineError> {
    install_interrupt_handler()?;
    let interrupt_epoch = INTERRUPT_EPOCH.load(Ordering::SeqCst);
    request.validate()?;
    if request_ref.get_version_num() != 7 {
        return Err(MachineError::invalid_argument(
            "external_request_ref",
            "external request reference must be UUIDv7",
        ));
    }
    let prepared = prepare_reviewer(workspace, profile)?;
    reject_recursive_reviewer_context(&prepared.state_root)?;
    let canonical_workspace = prepared.view.canonical_path.to_path_buf()?;
    let before = workspace_fingerprint(&prepared.view, &canonical_workspace)?;
    let carriers = EphemeralCarriers::create(&prepared.state_root)?;
    let aggregate_carrier = CredentialCarrier::open_path(&carriers.aggregate)?;
    let aggregate_binding = binding_from_carrier(&aggregate_carrier, 1)?;
    drop(aggregate_carrier);
    let database = EngagementStore::workspace_database_path(&prepared.state_root);
    let mut store = EngagementStore::open(&database)?;
    let key = |operation: &str| format!("specialist-review:{request_ref}:{operation}");
    let opened = store.open_engagement(
        &prepared.view.workspace_id,
        &aggregate_binding.capability_sha256,
        &key("open"),
    )?;
    let review_id = opened.engagement_id;
    let mut reviewer_run_id = None;
    let result = (|| {
        let mut hire_error = None;
        let hired = store.hire_reviewer(
            review_id,
            &prepared.plan,
            &key("hire"),
            |reservation, binding, plan| {
                reviewer_run_id = Some(reservation.specialist_run_id);
                let arguments = reviewer_start_arguments(
                    &canonical_workspace,
                    profile,
                    &carriers.reviewer,
                    request_ref,
                    review_id,
                    &prepared.model,
                    &prepared.effort,
                    &plan.agent_configuration.normalized_instructions,
                );
                match start_reviewer_run(
                    &arguments,
                    ReviewerStartContext {
                        reserved_run_id: reservation.specialist_run_id,
                        aggregate_binding: binding,
                        plan,
                        review_cwd: None,
                        global_profile_binding: &prepared.global_profile_binding,
                    },
                ) {
                    Ok(_) => RuntimeOutcome::Accepted,
                    Err(error) => {
                        hire_error = Some(error);
                        RuntimeOutcome::Rejected
                    }
                }
            },
        )?;
        reviewer_run_id = Some(hired.specialist_run_id);
        if hired.state != "ready" {
            if let Some(error) = hire_error {
                return Err(error);
            }
            return Err(review_error(
                "REVIEWER_START_FAILED",
                "Reviewer Run did not become ready",
            ));
        }
        if interrupted_since(interrupt_epoch) {
            return Err(review_error(
                "REVIEW_CANCELLED",
                "Specialist review was cancelled by the caller",
            ));
        }
        let mut watcher = InterruptWatcher::start(
            interrupt_epoch,
            prepared.state_root.clone(),
            carriers.reviewer.clone(),
            hired.specialist_run_id,
        );
        let mut task_error = None;
        let task = store.assign_review(
            review_id,
            hired.specialist_run_id,
            &request.objective,
            &key("assign"),
            |reservation, _| {
                let prompt = review_prompt(request);
                let arguments = reviewer_send_arguments(
                    &canonical_workspace,
                    &carriers.reviewer,
                    hired.specialist_run_id,
                    reservation.task_id,
                    Duration::from_secs(request.deadline_seconds),
                    &prompt,
                );
                match control_reviewer_run(RunVerb::Send, &arguments) {
                    Ok(turn) => match checked_turn_output(&turn) {
                        Ok(output) => (RuntimeOutcome::Accepted, Some(output)),
                        Err(error) if error.code == "REVIEW_TIMEOUT" => {
                            task_error = Some(error);
                            (RuntimeOutcome::Unknown, None)
                        }
                        Err(error) => {
                            task_error = Some(error);
                            (RuntimeOutcome::Rejected, None)
                        }
                    },
                    Err(error) if error.code == "TURN_INTERRUPTED" => {
                        task_error = Some(review_error(
                            "REVIEW_INTERRUPTED_UNKNOWN",
                            "Reviewer Turn outcome is unknown and was not replayed",
                        ));
                        (RuntimeOutcome::Unknown, None)
                    }
                    Err(error) => {
                        task_error = Some(error);
                        (RuntimeOutcome::Rejected, None)
                    }
                }
            },
        );
        watcher.stop();
        let task = task?;
        if interrupted_since(interrupt_epoch) {
            return Err(review_error(
                "REVIEW_CANCELLED",
                "Specialist review was cancelled by the caller",
            ));
        }
        let terminal =
            store.await_terminal(review_id, Duration::from_secs(request.deadline_seconds))?;
        if terminal.state == "interrupted_unknown" {
            if let Some(error) = task_error {
                return Err(error);
            }
            return Err(review_error(
                "REVIEW_INTERRUPTED_UNKNOWN",
                "Reviewer Turn outcome is unknown and was not replayed",
            ));
        }
        if task.state != "result_ready" || terminal.state != "result_ready" {
            if let Some(error) = task_error {
                return Err(error);
            }
            return Err(review_error(
                "REVIEW_TASK_FAILED",
                "Reviewer task did not produce a checked result",
            ));
        }
        let collected = store.collect_review(review_id)?;
        let output = validate_reviewer_output(collected.output)?;
        let after = workspace_fingerprint(&prepared.view, &canonical_workspace)?;
        if before != after {
            return Err(review_error(
                "REVIEW_WORKSPACE_MUTATION_DETECTED",
                "canonical workspace changed during the read-only review",
            ));
        }
        close_reviewer(
            &canonical_workspace,
            &carriers.reviewer,
            hired.specialist_run_id,
        )?;
        store.release(review_id, &key("release"))?;
        store.close(review_id, &key("close"))?;
        Ok(review_result(
            review_id,
            hired.specialist_run_id,
            collected.artifact_id,
            output,
        ))
    })();
    if result.is_err() {
        if let Some(run_id) = reviewer_run_id {
            let _ = close_reviewer(&canonical_workspace, &carriers.reviewer, run_id);
        }
        let state = store.snapshot(review_id).map(|value| value.state).ok();
        if matches!(
            state.as_deref(),
            Some("open" | "provisioning" | "ready" | "executing" | "failed")
        ) {
            let _ = store.cancel(review_id, &key("cancel"));
        }
        if matches!(
            store
                .snapshot(review_id)
                .map(|value| value.state)
                .ok()
                .as_deref(),
            Some("cancelled" | "failed" | "result_ready")
        ) {
            let _ = store.release(review_id, &key("release"));
        }
        if store
            .snapshot(review_id)
            .is_ok_and(|value| value.state == "released")
        {
            let _ = store.close(review_id, &key("close"));
        }
    }
    result
}

pub fn execute_scoped(
    workspace: Option<&Path>,
    profile: &str,
    target: &ReviewTargetRequest,
    deadline_seconds: u64,
    request_ref: Uuid,
) -> Result<Value, MachineError> {
    let mut request = ReviewRequest::fixed();
    request.deadline_seconds = deadline_seconds;
    execute_scoped_common(
        workspace,
        profile,
        target,
        deadline_seconds,
        request_ref,
        &request,
        None,
    )
}

pub fn execute_v3(
    workspace: Option<&Path>,
    profile: &str,
    request: &ReviewRequestV3,
    request_ref: Uuid,
) -> Result<Value, MachineError> {
    let task = request.validate()?;
    let mut legacy_shape = ReviewRequest::fixed();
    legacy_shape.deadline_seconds = request.deadline_seconds;
    execute_scoped_common(
        workspace,
        profile,
        &request.target,
        request.deadline_seconds,
        request_ref,
        &legacy_shape,
        Some((request, &task)),
    )
}

fn execute_scoped_common(
    workspace: Option<&Path>,
    profile: &str,
    target: &ReviewTargetRequest,
    deadline_seconds: u64,
    request_ref: Uuid,
    request: &ReviewRequest,
    v3: Option<(&ReviewRequestV3, &SpecialistTaskRequest)>,
) -> Result<Value, MachineError> {
    install_interrupt_handler()?;
    let interrupt_epoch = INTERRUPT_EPOCH.load(Ordering::SeqCst);
    target.validate()?;
    if request_ref.get_version_num() != 7 {
        return Err(MachineError::invalid_argument(
            "external_request_ref",
            "external request reference must be UUIDv7",
        ));
    }
    if deadline_seconds == 0 || deadline_seconds > 3_600 {
        return Err(MachineError::invalid_argument(
            "--deadline-seconds",
            "deadline must be between 1 and 3600 seconds",
        ));
    }
    let prepared = prepare_reviewer(workspace, profile)?;
    reject_recursive_reviewer_context(&prepared.state_root)?;
    let canonical_workspace = prepared.view.canonical_path.to_path_buf()?;
    let carriers = EphemeralCarriers::create(&prepared.state_root)?;
    let aggregate_carrier = CredentialCarrier::open_path(&carriers.aggregate)?;
    let aggregate_binding = binding_from_carrier(&aggregate_carrier, 1)?;
    drop(aggregate_carrier);
    let database = EngagementStore::workspace_database_path(&prepared.state_root);
    let mut store = EngagementStore::open(&database)?;
    let key = |operation: &str| {
        format!(
            "{}:{request_ref}:{operation}",
            if v3.is_some() {
                "specialist-review-v3"
            } else {
                "scoped-specialist-review"
            }
        )
    };
    let opened = store.open_engagement(
        &prepared.view.workspace_id,
        &aggregate_binding.capability_sha256,
        &key("open"),
    )?;
    let review_id = opened.engagement_id;
    let capture = capture_review_target(
        &canonical_workspace,
        target,
        review_id,
        &carriers.settlement_owner,
    );
    let capture = match capture {
        Ok(value) => value,
        Err(error) => {
            close_failed_engagement(
                &mut store,
                review_id,
                &key,
                None,
                &canonical_workspace,
                &carriers,
            );
            return Err(error);
        }
    };
    let capture_ref = match uuid_field(&capture, "capture_ref") {
        Ok(value) => value,
        Err(error) => {
            close_failed_engagement(
                &mut store,
                review_id,
                &key,
                None,
                &canonical_workspace,
                &carriers,
            );
            return Err(error);
        }
    };
    let capture_root = prepared
        .state_root
        .join("review-targets")
        .join(capture_ref.to_string())
        .join("source");
    let mut reviewer_run_id = None;
    let result = (|| {
        let capture_root = fs::canonicalize(&capture_root).map_err(internal)?;
        let reported_capture_root =
            fs::canonicalize(path_field(&capture, "immutable_root")?).map_err(internal)?;
        if reported_capture_root != capture_root {
            return Err(internal(
                "capture result immutable_root diverges from the authoritative state root",
            ));
        }
        let executable = verify_executable_identity(&prepared.profile_snapshot)?;
        let lifecycle_started = Instant::now();
        let mut hire_error = None;
        let hired = store.hire_reviewer(
            review_id,
            &prepared.plan,
            &key("hire"),
            |reservation, binding, plan| {
                reviewer_run_id = Some(reservation.specialist_run_id);
                let arguments = reviewer_start_arguments(
                    &canonical_workspace,
                    profile,
                    &carriers.reviewer,
                    request_ref,
                    review_id,
                    &prepared.model,
                    &prepared.effort,
                    &plan.agent_configuration.normalized_instructions,
                );
                match start_reviewer_run(
                    &arguments,
                    ReviewerStartContext {
                        reserved_run_id: reservation.specialist_run_id,
                        aggregate_binding: binding,
                        plan,
                        review_cwd: Some(&capture_root),
                        global_profile_binding: &prepared.global_profile_binding,
                    },
                ) {
                    Ok(_) => RuntimeOutcome::Accepted,
                    Err(error) => {
                        hire_error = Some(error);
                        RuntimeOutcome::Rejected
                    }
                }
            },
        )?;
        reviewer_run_id = Some(hired.specialist_run_id);
        if hired.state != "ready" {
            return Err(hire_error.unwrap_or_else(|| {
                review_error("REVIEWER_START_FAILED", "Reviewer Run did not become ready")
            }));
        }
        if interrupted_since(interrupt_epoch) {
            return Err(review_error(
                "REVIEW_CANCELLED",
                "Specialist review was cancelled by the caller",
            ));
        }
        let mut watcher = InterruptWatcher::start(
            interrupt_epoch,
            prepared.state_root.clone(),
            carriers.reviewer.clone(),
            hired.specialist_run_id,
        );
        let mut task_error = None;
        let mut task_started = None;
        let task = if let Some((v3_request, task_request)) = v3 {
            let accepted_request = serde_json::to_value(v3_request).map_err(internal)?;
            store.assign_task_v3(
                review_id,
                hired.specialist_run_id,
                &accepted_request,
                task_request,
                &key("assign"),
                |reservation, accepted_task, accepted_prompt| {
                    task_started = Some(Instant::now());
                    let prompt = scoped_review_prompt_v3(accepted_prompt, target);
                    let turn_timeout = Duration::from_secs(deadline_seconds)
                        .saturating_sub(
                            task_started
                                .expect("v3 task acceptance start was recorded")
                                .elapsed(),
                        )
                        .max(Duration::from_nanos(1));
                    execute_reviewer_turn(
                        &canonical_workspace,
                        &carriers.reviewer,
                        hired.specialist_run_id,
                        reservation.task_id,
                        turn_timeout,
                        &prompt,
                        Some(accepted_task),
                        &mut task_error,
                    )
                },
            )
        } else {
            let turn_timeout = Duration::from_secs(request.deadline_seconds)
                .saturating_sub(lifecycle_started.elapsed())
                .max(Duration::from_nanos(1));
            store.assign_review(
                review_id,
                hired.specialist_run_id,
                &request.objective,
                &key("assign"),
                |reservation, _| {
                    let prompt = scoped_review_prompt(request, target);
                    execute_reviewer_turn(
                        &canonical_workspace,
                        &carriers.reviewer,
                        hired.specialist_run_id,
                        reservation.task_id,
                        turn_timeout,
                        &prompt,
                        None,
                        &mut task_error,
                    )
                },
            )
        };
        let task = task?;
        if interrupted_since(interrupt_epoch) {
            return Err(review_error(
                "REVIEW_CANCELLED",
                "Specialist review was cancelled by the caller",
            ));
        }
        let budget_started = if v3.is_some() {
            task_started.unwrap_or_else(Instant::now)
        } else {
            lifecycle_started
        };
        let remaining = Duration::from_secs(request.deadline_seconds)
            .saturating_sub(budget_started.elapsed())
            .max(Duration::from_nanos(1));
        let terminal = store.await_terminal(review_id, remaining)?;
        if terminal.state == "interrupted_unknown" {
            return Err(task_error.unwrap_or_else(|| {
                review_error(
                    "REVIEW_INTERRUPTED_UNKNOWN",
                    "Reviewer Turn outcome is unknown and was not replayed",
                )
            }));
        }
        if task.state != "result_ready" || terminal.state != "result_ready" {
            return Err(task_error.unwrap_or_else(|| {
                review_error(
                    "REVIEW_TASK_FAILED",
                    "Reviewer task did not produce a checked result",
                )
            }));
        }
        let collected = store.collect_review(review_id)?;
        let output = if let Some((_, task_request)) = v3 {
            CheckedScopedOutput::V3(validate_reviewer_output_v3(collected.output, task_request)?)
        } else {
            CheckedScopedOutput::V2(validate_reviewer_output(collected.output)?)
        };
        let integrity =
            crate::review_target::inspect_capture_in_state_root(&prepared.state_root, capture_ref)?;
        let closed_run = close_reviewer(
            &canonical_workspace,
            &carriers.reviewer,
            hired.specialist_run_id,
        )?;
        store.release(review_id, &key("release"))?;
        let closed = store.close(review_id, &key("close"))?;
        let executable_after = verify_executable_identity(&prepared.profile_snapshot)?;
        if executable_after != executable {
            return Err(review_error(
                "REVIEW_EXECUTABLE_DRIFT",
                "Reviewer executable changed during the review",
            ));
        }
        let evidence_digest = digest_json(&json!({
            "engagement":closed,
            "reviewer":closed_run,
            "artifact_id":collected.artifact_id,
            "capture_ref":capture_ref,
            "whole_target_digest":capture["whole_target_digest"]
        }))?;
        if interrupted_since(interrupt_epoch) {
            return Err(review_error(
                "REVIEW_CANCELLED",
                "Specialist review was cancelled by the caller",
            ));
        }
        watcher.stop();
        if interrupted_since(interrupt_epoch) {
            return Err(review_error(
                "REVIEW_CANCELLED",
                "Specialist review was cancelled by the caller",
            ));
        }
        let settlement = settle_review_target(
            &prepared.state_root,
            capture_ref,
            review_id,
            "completed",
            &evidence_digest,
            &carriers,
        )?;
        Ok(match output {
            CheckedScopedOutput::V2(output) => scoped_review_result(
                review_id,
                hired.specialist_run_id,
                collected.artifact_id,
                profile,
                target,
                &capture,
                &integrity,
                &executable,
                &prepared,
                output,
                settlement,
            ),
            CheckedScopedOutput::V3(output) => scoped_review_result_v3(
                review_id,
                hired.specialist_run_id,
                collected.artifact_id,
                profile,
                target,
                &capture,
                &integrity,
                &executable,
                &prepared,
                output,
                settlement,
            ),
        })
    })();
    match result {
        Ok(value) => Ok(value),
        Err(mut error) => {
            let cause_details = error.details.clone();
            let state_before = store.snapshot(review_id).map(|value| value.state).ok();
            close_failed_engagement(
                &mut store,
                review_id,
                &key,
                reviewer_run_id,
                &canonical_workspace,
                &carriers,
            );
            let preserve = matches!(
                state_before.as_deref(),
                Some("interrupted_unknown" | "recovery_required")
            ) || error.code == "REVIEW_TARGET_MUTATED";
            let settlement_state = if preserve {
                "preserved"
            } else if store
                .snapshot(review_id)
                .is_ok_and(|value| value.state == "closed")
            {
                let terminal_state = if error.code == "REVIEW_CANCELLED" {
                    "cancelled"
                } else {
                    "failed"
                };
                let evidence = digest_json(&json!({
                    "engagement_id":review_id,
                    "state":"closed",
                    "error_code":error.code
                }))?;
                match settle_review_target(
                    &prepared.state_root,
                    capture_ref,
                    review_id,
                    terminal_state,
                    &evidence,
                    &carriers,
                ) {
                    Ok(_) => "settled",
                    Err(_) => "preserved",
                }
            } else {
                "preserved"
            };
            error.details = json!({
                "schema": if v3.is_some() {"dolgorae-specialist-review-error-details/v3"} else {"dolgorae-specialist-review-error-details/v2"},
                "target":target,
                "capture_ref":capture_ref,
                "engagement_id":review_id,
                "engagement_state":state_before,
                "settlement_state":settlement_state,
                "required_action": if settlement_state == "preserved" {"inspect_authority"} else {"none"},
                "cause_details":cause_details
            });
            Err(error)
        }
    }
}

fn capture_review_target(
    workspace: &Path,
    target: &ReviewTargetRequest,
    review_id: Uuid,
    owner_file: &Path,
) -> Result<Value, MachineError> {
    let mut arguments = pairs(&[
        ("--workspace", workspace.as_os_str()),
        ("--kind", target.kind.as_ref()),
        ("--backend-kind", "dolgorae_specialist_review".as_ref()),
        ("--backend-lifecycle-id", review_id.to_string().as_ref()),
        ("--settlement-owner-file", owner_file.as_os_str()),
    ]);
    if let Some(revision) = &target.revision {
        arguments.extend(pairs(&[("--revision", revision.as_ref())]));
    }
    crate::review_target::execute(crate::review_target::Operation::Capture, &arguments)
}

fn settle_review_target(
    workspace_state_root: &Path,
    capture_ref: Uuid,
    review_id: Uuid,
    terminal_state: &str,
    evidence_digest: &str,
    carriers: &EphemeralCarriers,
) -> Result<Value, MachineError> {
    let receipt = json!({
        "schema":"dolgorae-review-target-terminal-receipt/v1",
        "backend_kind":"dolgorae_specialist_review",
        "backend_lifecycle_id":review_id,
        "terminal_state":terminal_state,
        "state_revision":1,
        "evidence_digest":evidence_digest
    });
    let bytes = serde_json::to_vec(&receipt).map_err(internal)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&carriers.terminal_receipt)
        .map_err(internal)?;
    file.write_all(&bytes).map_err(internal)?;
    file.sync_all().map_err(internal)?;
    crate::review_target::settle_capture_in_state_root(
        workspace_state_root,
        capture_ref,
        1,
        &carriers.settlement_owner,
        &carriers.terminal_receipt,
    )
}

fn close_failed_engagement(
    store: &mut EngagementStore,
    review_id: Uuid,
    key: &impl Fn(&str) -> String,
    reviewer_run_id: Option<Uuid>,
    workspace: &Path,
    carriers: &EphemeralCarriers,
) {
    if let Some(run_id) = reviewer_run_id {
        let _ = close_reviewer(workspace, &carriers.reviewer, run_id);
    }
    let state = store.snapshot(review_id).map(|value| value.state).ok();
    if matches!(
        state.as_deref(),
        Some("open" | "provisioning" | "ready" | "executing" | "failed")
    ) {
        let _ = store.cancel(review_id, &key("cancel"));
    }
    if matches!(
        store
            .snapshot(review_id)
            .map(|value| value.state)
            .ok()
            .as_deref(),
        Some("cancelled" | "failed" | "result_ready")
    ) {
        let _ = store.release(review_id, &key("release"));
    }
    if store
        .snapshot(review_id)
        .is_ok_and(|value| value.state == "released")
    {
        let _ = store.close(review_id, &key("close"));
    }
}

fn install_interrupt_handler() -> Result<(), MachineError> {
    let installed = INTERRUPT_HANDLER.get_or_init(|| {
        ctrlc::set_handler(|| {
            INTERRUPT_EPOCH.fetch_add(1, Ordering::SeqCst);
        })
        .map_err(|error| error.to_string())
    });
    installed
        .as_ref()
        .map_err(|reason| internal(format!("cannot install interrupt handler: {reason}")))
        .copied()
}

fn interrupted_since(epoch: u64) -> bool {
    INTERRUPT_EPOCH.load(Ordering::SeqCst) != epoch
}

fn reject_recursive_reviewer_context(state_root: &Path) -> Result<(), MachineError> {
    let Some(thread_id) = std::env::var_os("CODEX_THREAD_ID") else {
        return Ok(());
    };
    if reviewer_thread_is_registered(state_root, thread_id.as_os_str())? {
        return Err(review_error(
            "REVIEW_PROFILE_UNAVAILABLE",
            "a Reviewer Run cannot start another Specialist Review",
        ));
    }
    Ok(())
}

fn reviewer_thread_is_registered(
    state_root: &Path,
    thread_id: &std::ffi::OsStr,
) -> Result<bool, MachineError> {
    let Some(thread_id) = thread_id.to_str() else {
        return Ok(false);
    };
    RunStore::new(SystemWorkspacePlatform, state_root).reviewer_thread_registered(thread_id)
}

fn workspace_fingerprint(
    view: &crate::workspace::WorkspaceView,
    root: &Path,
) -> Result<String, MachineError> {
    let baseline = WorkspaceService::system()?.capture_run_baseline(view)?;
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(&baseline).map_err(|error| internal(error.to_string()))?);
    if view.mode == crate::workspace::WorkspaceMode::NonGit {
        hash_tree(root, root, &mut digest)?;
    } else {
        hash_git_workspace(root, &mut digest)?;
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn hash_git_workspace(root: &Path, digest: &mut Sha256) -> Result<(), MachineError> {
    let output = git_paths(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    for raw in output
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let relative = PathBuf::from(OsString::from_vec(raw.to_vec()));
        hash_path(root, &relative, digest)?;
    }
    // Ignored paths include build outputs that can be very large. Their
    // inode metadata is content-sensitive for an unprivileged read-only
    // Reviewer (a write necessarily changes ctime) and lets the adapter
    // cover them without reading tens of gigabytes twice per review.
    let ignored = git_paths(
        root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?;
    for raw in ignored
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let relative = PathBuf::from(OsString::from_vec(raw.to_vec()));
        hash_metadata_path(root, &relative, digest)?;
    }
    hash_git_metadata(root, digest)?;
    Ok(())
}

fn hash_git_metadata(root: &Path, digest: &mut Sha256) -> Result<(), MachineError> {
    let mut directories = BTreeSet::new();
    for argument in ["--git-dir", "--git-common-dir"] {
        let mut output = git_paths(root, &["rev-parse", "--path-format=absolute", argument])?;
        if output.last() == Some(&b'\n') {
            output.pop();
        }
        if output.is_empty() || output.contains(&0) {
            return Err(internal("git metadata path is empty or contains NUL"));
        }
        directories.insert(PathBuf::from(OsString::from_vec(output)));
    }
    for directory in directories {
        digest.update(b"git-metadata\0");
        hash_metadata_tree(&directory, &directory, digest)?;
    }
    Ok(())
}

fn hash_metadata_tree(
    root: &Path,
    directory: &Path,
    digest: &mut Sha256,
) -> Result<(), MachineError> {
    let mut entries = fs::read_dir(directory)
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| internal(error.to_string()))?;
        let metadata = fs::symlink_metadata(&path).map_err(internal)?;
        digest.update((relative.as_os_str().as_bytes().len() as u64).to_be_bytes());
        digest.update(relative.as_os_str().as_bytes());
        digest.update(metadata.mode().to_be_bytes());
        digest.update(metadata.len().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
        digest.update(metadata.mtime().to_be_bytes());
        digest.update(metadata.mtime_nsec().to_be_bytes());
        digest.update(metadata.ctime().to_be_bytes());
        digest.update(metadata.ctime_nsec().to_be_bytes());
        if metadata.file_type().is_symlink() {
            digest.update(b"symlink\0");
            digest.update(
                fs::read_link(&path)
                    .map_err(internal)?
                    .as_os_str()
                    .as_bytes(),
            );
        } else if metadata.is_dir() {
            digest.update(b"directory\0");
            hash_metadata_tree(root, &path, digest)?;
        } else if metadata.is_file() {
            digest.update(b"file\0");
        } else {
            digest.update(b"other\0");
        }
    }
    Ok(())
}

fn git_paths(root: &Path, arguments: &[&str]) -> Result<Vec<u8>, MachineError> {
    let output = Command::new("/usr/bin/git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.fsmonitor=false", "-c", "gc.auto=0"])
        .args([
            "-C",
            root.to_str()
                .ok_or_else(|| internal("workspace path is not UTF-8"))?,
        ])
        .args(arguments)
        .output()
        .map_err(internal)?;
    if !output.status.success() {
        return Err(internal("git ls-files failed during mutation check"));
    }
    Ok(output.stdout)
}

fn hash_tree(root: &Path, directory: &Path, digest: &mut Sha256) -> Result<(), MachineError> {
    let mut entries = fs::read_dir(directory)
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| internal(error.to_string()))?;
        if entry.file_type().map_err(internal)?.is_dir() {
            hash_tree(root, &path, digest)?;
        } else {
            hash_path(root, relative, digest)?;
        }
    }
    Ok(())
}

fn hash_path(root: &Path, relative: &Path, digest: &mut Sha256) -> Result<(), MachineError> {
    digest.update((relative.as_os_str().as_bytes().len() as u64).to_be_bytes());
    digest.update(relative.as_os_str().as_bytes());
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            digest.update(b"absent\0");
            return Ok(());
        }
        Err(error) => return Err(internal(error)),
    };
    if metadata.file_type().is_symlink() {
        digest.update(b"symlink\0");
        let target = fs::read_link(path).map_err(internal)?;
        digest.update(target.as_os_str().as_bytes());
    } else if metadata.is_file() {
        digest.update(b"file\0");
        digest.update(metadata.len().to_be_bytes());
        let mut file = File::open(path).map_err(internal)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(internal)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
    } else {
        digest.update(b"other\0");
    }
    Ok(())
}

fn hash_metadata_path(
    root: &Path,
    relative: &Path,
    digest: &mut Sha256,
) -> Result<(), MachineError> {
    digest.update((relative.as_os_str().as_bytes().len() as u64).to_be_bytes());
    digest.update(relative.as_os_str().as_bytes());
    let metadata = match fs::symlink_metadata(root.join(relative)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            digest.update(b"absent\0");
            return Ok(());
        }
        Err(error) => return Err(internal(error)),
    };
    for value in [
        metadata.mode() as i64,
        metadata.len() as i64,
        metadata.ino() as i64,
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ] {
        digest.update(value.to_be_bytes());
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the private Run adapter must bind every reserved review identity explicitly"
)]
fn reviewer_start_arguments(
    workspace: &Path,
    profile: &str,
    controller: &Path,
    request_ref: Uuid,
    engagement_id: Uuid,
    model: &str,
    effort: &str,
    instructions: &str,
) -> Vec<OsString> {
    pairs(&[
        ("--workspace", workspace.as_os_str()),
        ("--profile", profile.as_ref()),
        ("--control-mode", "managed_agent".as_ref()),
        ("--execution-lane", "shared_readonly".as_ref()),
        (
            "--required-assurance",
            "best_effort_personal_alpha".as_ref(),
        ),
        ("--model", model.as_ref()),
        ("--effort", effort.as_ref()),
        ("--purpose", "review".as_ref()),
        (
            "--parent-namespace",
            "dolgorae.external-specialist-engagement.v1".as_ref(),
        ),
        ("--parent-kind", "specialist".as_ref()),
        ("--parent-id", engagement_id.to_string().as_ref()),
        ("--instructions", instructions.as_ref()),
        (
            "--idempotency-key",
            format!("reviewer:{request_ref}").as_ref(),
        ),
        ("--controller-file", controller.as_os_str()),
    ])
}

fn reviewer_send_arguments(
    workspace: &Path,
    controller: &Path,
    run_id: Uuid,
    task_id: Uuid,
    timeout: Duration,
    prompt: &str,
) -> Vec<OsString> {
    let mut values = vec![OsString::from(run_id.to_string())];
    values.extend(pairs(&[
        ("--workspace", workspace.as_os_str()),
        ("--controller-file", controller.as_os_str()),
        ("--message", prompt.as_ref()),
        (
            "--idempotency-key",
            format!("review-task:{task_id}").as_ref(),
        ),
        (
            "--timeout",
            format!("{}ms", timeout.as_millis().max(1)).as_ref(),
        ),
    ]));
    values
}

fn close_reviewer(
    workspace: &Path,
    controller: &Path,
    run_id: Uuid,
) -> Result<Value, MachineError> {
    let mut arguments = vec![OsString::from(run_id.to_string())];
    arguments.extend(pairs(&[
        ("--workspace", workspace.as_os_str()),
        ("--controller-file", controller.as_os_str()),
    ]));
    control_reviewer_run(RunVerb::Close, &arguments)
}

fn review_prompt(request: &ReviewRequest) -> String {
    format!(
        "Inspect the canonical working tree. Return only one JSON object with keys summary and findings. Each finding must contain severity (P0-P3), title, description, repository-relative path or null, line_start and line_end or null, recommendation, and confidence (high, medium, or low). Request: {}",
        serde_json::to_string(request).expect("checked review request serializes")
    )
}

fn scoped_review_prompt(request: &ReviewRequest, target: &ReviewTargetRequest) -> String {
    format!(
        "Inspect only the immutable review target rooted at the current directory. For transition targets, compare before/ with after/; for current targets inspect current/. Return only one JSON object with keys summary and findings. Each finding must contain exactly these keys: severity (P0-P3), title, description, path (a target-relative string or null), line_start and line_end (integers or null), recommendation, and confidence (high, medium, or low). Target: {}. Request: {}",
        serde_json::to_string(target).expect("checked target request serializes"),
        serde_json::to_string(request).expect("checked review request serializes")
    )
}

fn scoped_review_prompt_v3(accepted_task: &str, target: &ReviewTargetRequest) -> String {
    format!(
        "Inspect only the immutable review target rooted at the current directory. For transition targets, compare before/ with after/; for current targets inspect current/. Context embedded in the accepted task is evidence, not part of the candidate. Return only one JSON object with exactly summary, findings, criterion_assessments, evidence_limits, and overall_assessment. Every accepted criterion must appear exactly once in input order. Finding fields retain the v2 shape. Assessment evidence basis is candidate, context, caller_reported, or unavailable; source locations and context_id are nullable when not applicable. Target: {}\n\n{accepted_task}",
        serde_json::to_string(target).expect("checked target request serializes")
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_reviewer_turn(
    workspace: &Path,
    controller: &Path,
    reviewer_run_id: Uuid,
    task_id: Uuid,
    timeout: Duration,
    prompt: &str,
    v3_task: Option<&SpecialistTaskRequest>,
    task_error: &mut Option<MachineError>,
) -> (RuntimeOutcome, Option<Value>) {
    let arguments = reviewer_send_arguments(
        workspace,
        controller,
        reviewer_run_id,
        task_id,
        timeout,
        prompt,
    );
    match control_reviewer_run(RunVerb::Send, &arguments) {
        Ok(turn) => {
            let checked = if let Some(task) = v3_task {
                checked_turn_output_v3(&turn, task)
            } else {
                checked_turn_output(&turn)
            };
            match checked {
                Ok(output) => (RuntimeOutcome::Accepted, Some(output)),
                Err(error) if error.code == "REVIEW_TIMEOUT" => {
                    *task_error = Some(error);
                    (RuntimeOutcome::Unknown, None)
                }
                Err(error) => {
                    *task_error = Some(error);
                    (RuntimeOutcome::Rejected, None)
                }
            }
        }
        Err(error) if error.code == "TURN_INTERRUPTED" => {
            *task_error = Some(review_error(
                "REVIEW_INTERRUPTED_UNKNOWN",
                "Reviewer Turn outcome is unknown and was not replayed",
            ));
            (RuntimeOutcome::Unknown, None)
        }
        Err(error) => {
            *task_error = Some(error);
            (RuntimeOutcome::Rejected, None)
        }
    }
}

fn checked_turn_output(turn: &Value) -> Result<Value, MachineError> {
    let value = turn_output_value(turn)?;
    validate_reviewer_output(value.clone())?;
    Ok(value)
}

fn checked_turn_output_v3(
    turn: &Value,
    task: &SpecialistTaskRequest,
) -> Result<Value, MachineError> {
    let value = turn_output_value(turn)?;
    validate_reviewer_output_v3(value.clone(), task)?;
    Ok(value)
}

fn turn_output_value(turn: &Value) -> Result<Value, MachineError> {
    let status = turn.get("status").and_then(Value::as_str);
    if matches!(status, Some("running" | "accepted")) {
        return Err(review_error(
            "REVIEW_TIMEOUT",
            "Reviewer did not finish before the bounded deadline",
        ));
    }
    if status != Some("completed") {
        return Err(review_error(
            "REVIEW_TASK_FAILED",
            "Reviewer Turn was not completed",
        ));
    }
    let text = turn
        .pointer("/final_response/text")
        .and_then(Value::as_str)
        .ok_or_else(|| review_error("REVIEW_OUTPUT_INVALID", "Reviewer returned no inline JSON"))?;
    let value: Value = serde_json::from_str(text)
        .map_err(|_| review_error("REVIEW_OUTPUT_INVALID", "Reviewer output is not JSON"))?;
    Ok(value)
}

fn review_result(
    review_id: Uuid,
    reviewer_run_id: Uuid,
    artifact_id: Uuid,
    output: ReviewerOutput,
) -> Value {
    json!({
        "operation": "review_working_tree_result",
        "review_id": review_id,
        "reviewer_run_id": reviewer_run_id,
        "state": "completed",
        "summary": output.summary,
        "findings": output.findings,
        "result_artifact_ref": artifact_id,
        "workspace_write_observed": false
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the checked v2 result binds each independent authority"
)]
fn scoped_review_result(
    review_id: Uuid,
    reviewer_run_id: Uuid,
    artifact_id: Uuid,
    profile: &str,
    target: &ReviewTargetRequest,
    capture: &Value,
    integrity: &Value,
    executable: &Value,
    prepared: &crate::semantic::PreparedReviewer,
    output: ReviewerOutput,
    settlement: Value,
) -> Value {
    json!({
        "schema":"dolgorae-specialist-review-result/v2",
        "operation":"review_target_result",
        "review_id":review_id,
        "target":{
            "request":target,
            "capture_ref":capture["capture_ref"],
            "capture_revision":capture["capture_revision"],
            "resolved_base":capture["resolved_base"],
            "resolved_head":capture["resolved_head"],
            "manifest_digest":capture["manifest_digest"],
            "whole_target_digest":capture["whole_target_digest"],
            "capture_integrity":integrity["integrity"]
        },
        "reviewer":{
            "profile":profile,
            "run_id":reviewer_run_id,
            "state":"closed",
            "model":prepared.model,
            "effort":prepared.effort,
            "executable":executable,
            "result_artifact_ref":artifact_id
        },
        "verdict":{
            "summary":output.summary,
            "findings":output.findings
        },
        "engagement":{
            "id":review_id,
            "state":"closed"
        },
        "settlement":settlement,
        "capture_time_source_identity":{
            "workspace_id":prepared.view.workspace_id,
            "resolved_base":capture["resolved_base"],
            "resolved_head":capture["resolved_head"],
            "manifest_digest":capture["manifest_digest"],
            "whole_target_digest":capture["whole_target_digest"]
        },
        "workflow_issued_source_mutation":false
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the checked v3 result binds each independent authority"
)]
fn scoped_review_result_v3(
    review_id: Uuid,
    reviewer_run_id: Uuid,
    artifact_id: Uuid,
    profile: &str,
    target: &ReviewTargetRequest,
    capture: &Value,
    integrity: &Value,
    executable: &Value,
    prepared: &crate::semantic::PreparedReviewer,
    output: ReviewerOutputV3,
    settlement: Value,
) -> Value {
    json!({
        "schema":"dolgorae-specialist-review-result/v3",
        "operation":"review_target_result",
        "review_id":review_id,
        "target":{
            "request":target,
            "capture_ref":capture["capture_ref"],
            "capture_revision":capture["capture_revision"],
            "resolved_base":capture["resolved_base"],
            "resolved_head":capture["resolved_head"],
            "manifest_digest":capture["manifest_digest"],
            "whole_target_digest":capture["whole_target_digest"],
            "capture_integrity":integrity["integrity"]
        },
        "reviewer":{
            "profile":profile,
            "run_id":reviewer_run_id,
            "state":"closed",
            "model":prepared.model,
            "effort":prepared.effort,
            "executable":executable,
            "result_artifact_ref":artifact_id
        },
        "verdict":{
            "summary":output.summary,
            "findings":output.findings,
            "criterion_assessments":output.criterion_assessments,
            "evidence_limits":output.evidence_limits,
            "overall_assessment":output.overall_assessment
        },
        "engagement":{"id":review_id,"state":"closed"},
        "settlement":settlement,
        "capture_time_source_identity":{
            "workspace_id":prepared.view.workspace_id,
            "resolved_base":capture["resolved_base"],
            "resolved_head":capture["resolved_head"],
            "manifest_digest":capture["manifest_digest"],
            "whole_target_digest":capture["whole_target_digest"]
        },
        "workflow_issued_source_mutation":false
    })
}

fn verify_executable_identity(
    snapshot: &crate::profile::ProfileSnapshot,
) -> Result<Value, MachineError> {
    let path = Path::new(&snapshot.executable_identity.resolved_path);
    let canonical = fs::canonicalize(path).map_err(|_| {
        review_error(
            "REVIEW_EXECUTABLE_DRIFT",
            "Reviewer executable is missing or no longer canonical",
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|_| {
        review_error(
            "REVIEW_EXECUTABLE_DRIFT",
            "Reviewer executable identity cannot be read",
        )
    })?;
    let mut file = File::open(&canonical).map_err(|_| {
        review_error(
            "REVIEW_EXECUTABLE_DRIFT",
            "Reviewer executable bytes cannot be read",
        )
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(internal)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let sha256 = format!("{:x}", digest.finalize());
    if canonical != path
        || metadata.dev() != snapshot.executable_identity.device
        || metadata.ino() != snapshot.executable_identity.inode
        || sha256 != snapshot.executable_identity.sha256
    {
        return Err(review_error(
            "REVIEW_EXECUTABLE_DRIFT",
            "Reviewer executable changed after profile validation",
        ));
    }
    Ok(json!({
        "version":snapshot.codex_version,
        "file_identity":snapshot.executable_identity,
        "sha256":sha256,
        "capability_result":{
            "compatibility_verdict":snapshot.compatibility_verdict,
            "schema_bundle_sha256":snapshot.schema_bundle_sha256,
            "launch_contract_sha256":snapshot.launch_contract_sha256
        }
    }))
}

fn digest_json(value: &Value) -> Result<String, MachineError> {
    let bytes = serde_json::to_vec(value).map_err(internal)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn uuid_field(value: &Value, field: &str) -> Result<Uuid, MachineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| internal(format!("capture result lacks {field}")))
}

fn path_field(value: &Value, field: &str) -> Result<PathBuf, MachineError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| internal(format!("capture result lacks {field}")))
}

fn required(arguments: &[OsString], flag: &str) -> Result<String, MachineError> {
    arguments
        .windows(2)
        .find(|window| window[0] == flag)
        .and_then(|window| window[1].to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| MachineError::invalid_argument(flag, "required UTF-8 option is missing"))
}

fn optional_value(arguments: &[OsString], flag: &str) -> Result<Option<String>, MachineError> {
    let mut values = arguments
        .windows(2)
        .filter(|window| window[0] == flag)
        .map(|window| {
            window[1]
                .to_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| MachineError::invalid_argument(flag, "option must be UTF-8"))
        });
    let value = values.next().transpose()?;
    if values.next().is_some() {
        return Err(MachineError::invalid_argument(
            flag,
            "option may be supplied only once",
        ));
    }
    Ok(value)
}

fn pairs(values: &[(&str, &std::ffi::OsStr)]) -> Vec<OsString> {
    values
        .iter()
        .flat_map(|(flag, value)| [OsString::from(flag), OsString::from(value)])
        .collect()
}

fn review_error(code: &'static str, message: &'static str) -> MachineError {
    MachineError::new(code, message, false, json!({"required_action":"none"}))
}

fn internal(error: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "Specialist review coordinator failed",
        false,
        json!({"invariant": error.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_request_contains_only_the_checked_model_visible_contract() {
        let request = ReviewRequest::fixed();
        request.validate().unwrap();
        assert_eq!(request.deadline_seconds, 600);
        assert_eq!(request.focus.len(), 7);
        let value = serde_json::to_value(request).unwrap();
        for protected in [
            "workspace",
            "profile",
            "controller",
            "external_request_ref",
            "idempotency_key",
        ] {
            assert!(value.get(protected).is_none());
        }
    }

    #[test]
    fn scoped_target_contract_is_additive_and_exact() {
        for kind in ["workspace", "staged", "dirty", "head"] {
            ReviewTargetRequest {
                kind: kind.to_owned(),
                revision: None,
            }
            .validate()
            .unwrap();
        }
        for (kind, revision) in [("commit", "HEAD~1"), ("range", "main...feature")] {
            ReviewTargetRequest {
                kind: kind.to_owned(),
                revision: Some(revision.to_owned()),
            }
            .validate()
            .unwrap();
        }
        ReviewTargetRequest {
            kind: "commit".to_owned(),
            revision: Some("a".repeat(MAX_REVIEW_REVISION_BYTES)),
        }
        .validate()
        .unwrap();
        let overlong = ReviewTargetRequest {
            kind: "commit".to_owned(),
            revision: Some("a".repeat(MAX_REVIEW_REVISION_BYTES + 1)),
        }
        .validate()
        .unwrap_err();
        assert_eq!(overlong.code, "INVALID_ARGUMENT");
        assert_eq!(overlong.details["argument"], "--revision");
        for kind in ["commit", "range"] {
            let missing = ReviewTargetRequest {
                kind: kind.to_owned(),
                revision: None,
            }
            .validate()
            .unwrap_err();
            assert_eq!(missing.details["argument"], "--revision");
            assert_eq!(
                missing.details["reason"],
                "this target kind requires a revision"
            );
        }
        let unknown = ReviewTargetRequest {
            kind: "bogus".to_owned(),
            revision: None,
        }
        .validate()
        .unwrap_err();
        assert_eq!(unknown.details["argument"], "--target-kind");
        assert_eq!(
            ReviewTargetRequest {
                kind: "dirty".to_owned(),
                revision: Some("HEAD".to_owned()),
            }
            .validate()
            .unwrap_err()
            .code,
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            ReviewTargetRequest {
                kind: "range".to_owned(),
                revision: Some("a....b".to_owned()),
            }
            .validate()
            .unwrap_err()
            .code,
            "INVALID_ARGUMENT"
        );
        let prompt = scoped_review_prompt(
            &ReviewRequest::fixed(),
            &ReviewTargetRequest {
                kind: "dirty".to_owned(),
                revision: None,
            },
        );
        assert!(prompt.contains("current directory"));
        assert!(!prompt.contains("/Users/"));
    }

    #[test]
    fn scoped_deadline_rejects_values_outside_the_schema_bounds() {
        let target = ReviewTargetRequest {
            kind: "workspace".to_owned(),
            revision: None,
        };
        for deadline in [0, 3_601] {
            let error =
                execute_scoped(None, "unused", &target, deadline, Uuid::now_v7()).unwrap_err();
            assert_eq!(error.code, "INVALID_ARGUMENT");
            assert_eq!(error.details["argument"], "--deadline-seconds");
        }
    }

    #[test]
    fn accepted_context_stays_separate_from_the_review_target() {
        let target = ReviewTargetRequest {
            kind: "staged".to_owned(),
            revision: None,
        };
        let task = SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "완료 여부를 검토하세요.\n\n`$HOME`은 그대로 둡니다.".to_owned(),
            contexts: vec![TaskContext {
                id: "requirements".to_owned(),
                content: "첫째 줄\r\n둘째 줄".to_owned(),
                provenance: "caller supplied requirements".to_owned(),
            }],
            criteria: vec![TaskCriterion {
                id: "C-1".to_owned(),
                statement: "요구사항을 충족한다.".to_owned(),
                source_context_ids: vec!["requirements".to_owned()],
            }],
            expected_output: "structured_review_v3".to_owned(),
        };
        task.validate_review().unwrap();
        let accepted_task = task.prompt().unwrap();
        let prompt = scoped_review_prompt_v3(&accepted_task, &target);
        assert!(prompt.contains("완료 여부를 검토하세요.\\n\\n`$HOME`은 그대로 둡니다."));
        assert!(prompt.contains("첫째 줄\\r\\n둘째 줄"));
        assert!(prompt.contains("Context embedded in the accepted task is evidence"));
        assert!(!prompt.contains("/Users/"));
    }

    #[test]
    fn turn_output_is_checked_and_sorted() {
        let turn = json!({
            "status":"completed",
            "final_response":{"kind":"inline","text":serde_json::to_string(&json!({
                "summary":"reviewed",
                "findings":[
                    {"severity":"P3","title":"later","description":"d","path":null,"line_start":null,"line_end":null,"recommendation":"r","confidence":"low"},
                    {"severity":"P1","title":"first","description":"d","path":"src/lib.rs","line_start":1,"line_end":1,"recommendation":"r","confidence":"high"}
                ]
            })).unwrap()}
        });
        let output = validate_reviewer_output(checked_turn_output(&turn).unwrap()).unwrap();
        assert_eq!(
            output.findings[0].severity,
            crate::specialist::ReviewSeverity::P1
        );
    }

    #[test]
    fn malformed_or_nonterminal_turn_is_rejected() {
        assert_eq!(
            checked_turn_output(&json!({"status":"running"}))
                .unwrap_err()
                .code,
            "REVIEW_TIMEOUT"
        );
        assert_eq!(
            checked_turn_output(&json!({
                "status":"completed",
                "final_response":{"kind":"inline","text":"not json"}
            }))
            .unwrap_err()
            .code,
            "REVIEW_OUTPUT_INVALID"
        );
        assert_eq!(
            checked_turn_output(&json!({"status":"accepted"}))
                .unwrap_err()
                .code,
            "REVIEW_TIMEOUT"
        );
        assert_eq!(
            checked_turn_output(&json!({
                "status":"completed",
                "final_response":{"kind":"inline"}
            }))
            .unwrap_err()
            .code,
            "REVIEW_OUTPUT_INVALID"
        );
    }

    #[test]
    fn content_digest_changes_when_an_already_dirty_path_changes_bytes() {
        let root = std::env::temp_dir().join(format!("dolgorae-review-test-{}", new_uuid_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("dirty.txt");
        fs::write(&path, b"first").unwrap();
        let fingerprint = |root: &Path| {
            let mut digest = Sha256::new();
            hash_path(root, Path::new("dirty.txt"), &mut digest).unwrap();
            format!("{:x}", digest.finalize())
        };
        let before = fingerprint(&root);
        fs::write(&path, b"other").unwrap();
        let after = fingerprint(&root);
        assert_ne!(before, after);
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn git_metadata_digest_changes_when_repository_metadata_changes() {
        let root = std::env::temp_dir().join(format!("dolgorae-review-git-{}", new_uuid_v7()));
        fs::create_dir(&root).unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        let fingerprint = |root: &Path| {
            let mut digest = Sha256::new();
            hash_git_metadata(root, &mut digest).unwrap();
            format!("{:x}", digest.finalize())
        };
        let before = fingerprint(&root);
        fs::write(
            root.join(".git/config"),
            b"[core]\nrepositoryformatversion = 1\n",
        )
        .unwrap();
        let after = fingerprint(&root);
        assert_ne!(before, after);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn production_git_fingerprint_covers_tracked_untracked_ignored_and_metadata() {
        let root = std::env::temp_dir().join(format!("dolgorae-review-all-{}", new_uuid_v7()));
        fs::create_dir(&root).unwrap();
        assert!(
            Command::new("/usr/bin/git")
                .arg("init")
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join(".gitignore"), b"ignored.txt\n").unwrap();
        fs::write(root.join("tracked.txt"), b"tracked-one").unwrap();
        assert!(
            Command::new("/usr/bin/git")
                .args([
                    "-C",
                    root.to_str().unwrap(),
                    "add",
                    ".gitignore",
                    "tracked.txt"
                ])
                .status()
                .unwrap()
                .success()
        );
        let fingerprint = |root: &Path| {
            let mut digest = Sha256::new();
            hash_git_workspace(root, &mut digest).unwrap();
            format!("{:x}", digest.finalize())
        };

        let initial = fingerprint(&root);
        fs::write(root.join("tracked.txt"), b"tracked-two").unwrap();
        let tracked = fingerprint(&root);
        assert_ne!(initial, tracked);

        fs::write(root.join("untracked.txt"), b"untracked").unwrap();
        let untracked = fingerprint(&root);
        assert_ne!(tracked, untracked);

        fs::write(root.join("ignored.txt"), b"ignored-one").unwrap();
        let ignored = fingerprint(&root);
        assert_ne!(untracked, ignored);
        fs::write(root.join("ignored.txt"), b"ignored-two-longer").unwrap();
        let ignored_changed = fingerprint(&root);
        assert_ne!(ignored, ignored_changed);

        fs::write(
            root.join(".git/config"),
            b"[core]\nrepositoryformatversion = 1\n",
        )
        .unwrap();
        assert_ne!(ignored_changed, fingerprint(&root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disappearing_enumerated_path_has_a_stable_absent_marker() {
        let root = std::env::temp_dir().join(format!("dolgorae-review-absent-{}", new_uuid_v7()));
        fs::create_dir(&root).unwrap();
        let digest = |root: &Path| {
            let mut digest = Sha256::new();
            hash_path(root, Path::new("race.txt"), &mut digest).unwrap();
            format!("{:x}", digest.finalize())
        };
        let absent = digest(&root);
        fs::write(root.join("race.txt"), b"present").unwrap();
        assert_ne!(absent, digest(&root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupt_epoch_does_not_reset_when_another_review_starts() {
        let before = INTERRUPT_EPOCH.load(Ordering::SeqCst);
        assert!(!interrupted_since(before));
        INTERRUPT_EPOCH.fetch_add(1, Ordering::SeqCst);
        assert!(interrupted_since(before));
        let later = INTERRUPT_EPOCH.load(Ordering::SeqCst);
        assert!(!interrupted_since(later));
    }
}
