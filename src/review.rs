//! One-shot read-only Specialist Review coordinator.

use crate::controller::{CredentialCarrier, binding_from_carrier, create_controller_credential};
use crate::domain::ControllerKind;
use crate::engagement::{EngagementStore, RuntimeOutcome};
use crate::machine::{MachineError, new_uuid_v7};
use crate::semantic::{
    ReviewerStartContext, RunVerb, control_reviewer_run, prepare_reviewer, start_reviewer_run,
};
use crate::specialist::{ReviewerOutput, validate_reviewer_output};
use crate::workspace::WorkspaceService;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read as _;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

pub const REVIEW_OBJECTIVE: &str = "Review the current working tree for concrete correctness, regression, concurrency, error-handling, security, test-coverage, and maintainability issues.";
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

impl ReviewRequest {
    #[must_use]
    pub fn fixed() -> Self {
        Self {
            operation: "review_working_tree".to_owned(),
            scope: "working_tree".to_owned(),
            objective: REVIEW_OBJECTIVE.to_owned(),
            focus: REVIEW_FOCUS.iter().map(ToString::to_string).collect(),
            expected_output: "structured_findings_v1".to_owned(),
            deadline_seconds: 600,
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
        })
    }
}

impl Drop for EphemeralCarriers {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.aggregate);
        let _ = fs::remove_file(&self.reviewer);
        let _ = fs::remove_dir(&self.root);
    }
}

pub fn execute_cli(arguments: &[OsString]) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(arguments, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let profile = required(arguments, "--profile")?;
    if required(arguments, "--scope")? != "working-tree" {
        return Err(MachineError::invalid_argument(
            "--scope",
            "the preview supports only working-tree",
        ));
    }
    if required(arguments, "--format")? != "json" {
        return Err(MachineError::invalid_argument(
            "--format",
            "the canonical specialist review format is json",
        ));
    }
    execute(
        workspace.as_deref(),
        &profile,
        &ReviewRequest::fixed(),
        new_uuid_v7(),
    )
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
    let prepared = prepare_reviewer(workspace, profile, &request.objective)?;
    reject_recursive_reviewer_context(&prepared.state_root)?;
    let canonical_workspace = prepared.view.canonical_path.to_path_buf()?;
    let before = workspace_fingerprint(&prepared.view, &canonical_workspace)?;
    let carriers = EphemeralCarriers::create(&prepared.state_root)?;
    let aggregate_carrier = CredentialCarrier::open_path(&carriers.aggregate)?;
    let aggregate_binding = binding_from_carrier(&aggregate_carrier, 1)?;
    drop(aggregate_carrier);
    let database = prepared
        .state_root
        .join("orchestration")
        .join("orchestration.sqlite3");
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
        let watcher_done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let watcher_done = Arc::clone(&watcher_done);
            let workspace = canonical_workspace.clone();
            let controller = carriers.reviewer.clone();
            let run_id = hired.specialist_run_id;
            thread::spawn(move || {
                while !watcher_done.load(Ordering::SeqCst) {
                    if interrupted_since(interrupt_epoch) {
                        let mut arguments = vec![OsString::from(run_id.to_string())];
                        arguments.extend(pairs(&[
                            ("--workspace", workspace.as_os_str()),
                            ("--controller-file", controller.as_os_str()),
                        ]));
                        let _ = control_reviewer_run(RunVerb::Interrupt, &arguments);
                        return;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            })
        };
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
                    request.deadline_seconds,
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
        watcher_done.store(true, Ordering::SeqCst);
        let _ = watcher.join();
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
    let runs = state_root.join("runs");
    let entries = match fs::read_dir(&runs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(internal(error)),
    };
    for entry in entries {
        let root = entry.map_err(internal)?.path();
        let state: Value = match fs::read(root.join("state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(internal)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(internal(error)),
        };
        if state.get("thread_id").and_then(Value::as_str) != Some(thread_id) {
            continue;
        }
        let manifest: Value =
            serde_json::from_slice(&fs::read(root.join("manifest.json")).map_err(internal)?)
                .map_err(internal)?;
        return Ok(manifest
            .pointer("/agent_configuration/role_reference")
            .and_then(Value::as_str)
            == Some(crate::specialist::REVIEWER_ROLE_REFERENCE)
            || manifest
                .pointer("/aggregate_binding/role_reference")
                .and_then(Value::as_str)
                == Some(crate::specialist::REVIEWER_ROLE_REFERENCE));
    }
    Ok(false)
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
    deadline_seconds: u64,
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
        ("--timeout", format!("{deadline_seconds}s").as_ref()),
    ]));
    values
}

fn close_reviewer(workspace: &Path, controller: &Path, run_id: Uuid) -> Result<(), MachineError> {
    let mut arguments = vec![OsString::from(run_id.to_string())];
    arguments.extend(pairs(&[
        ("--workspace", workspace.as_os_str()),
        ("--controller-file", controller.as_os_str()),
    ]));
    control_reviewer_run(RunVerb::Close, &arguments).map(|_| ())
}

fn review_prompt(request: &ReviewRequest) -> String {
    format!(
        "Inspect the canonical working tree. Return only one JSON object with keys summary and findings. Each finding must contain severity (P0-P3), title, description, repository-relative path or null, line_start and line_end or null, recommendation, and confidence (high, medium, or low). Request: {}",
        serde_json::to_string(request).expect("checked review request serializes")
    )
}

fn checked_turn_output(turn: &Value) -> Result<Value, MachineError> {
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
    validate_reviewer_output(value.clone())?;
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

fn required(arguments: &[OsString], flag: &str) -> Result<String, MachineError> {
    arguments
        .windows(2)
        .find(|window| window[0] == flag)
        .and_then(|window| window[1].to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| MachineError::invalid_argument(flag, "required UTF-8 option is missing"))
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
    fn registered_reviewer_thread_is_rejected_structurally() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-review-recursion-{}", new_uuid_v7()));
        let run = root.join("runs/reviewer");
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("state.json"),
            br#"{"thread_id":"thread-reviewer"}"#,
        )
        .unwrap();
        fs::write(
            run.join("manifest.json"),
            format!(
                "{{\"agent_configuration\":{{\"role_reference\":\"{}\"}}}}",
                crate::specialist::REVIEWER_ROLE_REFERENCE
            ),
        )
        .unwrap();
        assert!(
            reviewer_thread_is_registered(&root, std::ffi::OsStr::new("thread-reviewer")).unwrap()
        );
        assert!(
            !reviewer_thread_is_registered(&root, std::ffi::OsStr::new("thread-parent")).unwrap()
        );
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
