//! Durable one-shot receipts join existing review and lifecycle authorities.

use crate::controller::{CredentialCarrier, binding_from_carrier};
use crate::domain::RunLifecycle;
use crate::engagement::{
    EngagementStore, OneShotOperation, OneShotOperationInput, OneShotTerminal,
};
use crate::global_runtime::ResolvedGlobalProfile;
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::profile::{ReviewServerOwner, ReviewServerStatus};
use crate::review::{
    DurableReviewContext, EphemeralCarriers, ReviewRequest, ReviewRequestV3, ReviewTargetRequest,
};
use crate::run::RunStore;
use crate::workspace::{SystemWorkspacePlatform, WorkspaceService, WorkspaceView};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) enum Invocation {
    Legacy(ReviewRequest),
    Scoped(ReviewTargetRequest, u64),
    Task(ReviewRequestV3),
}

impl Invocation {
    fn request(&self) -> Result<Value, MachineError> {
        match self {
            Self::Legacy(request) => {
                request.validate()?;
                serde_json::to_value(request).map_err(internal)
            }
            Self::Scoped(target, deadline) => {
                target.validate()?;
                if !(1..=3600).contains(deadline) {
                    return Err(MachineError::invalid_argument(
                        "deadline_seconds",
                        "deadline must be between 1 and 3600 seconds",
                    ));
                }
                Ok(
                    json!({"operation":"review_target", "target":target, "deadline_seconds":deadline}),
                )
            }
            Self::Task(request) => {
                request.validate()?;
                serde_json::to_value(request).map_err(internal)
            }
        }
    }
}

struct OperationGuard(File);
impl OperationGuard {
    fn path(root: &Path, reference: Uuid) -> PathBuf {
        root.join("orchestration")
            .join(format!(".one-shot-{reference}.lock"))
    }
    fn acquire(root: &Path, reference: Uuid) -> Result<Self, MachineError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(Self::path(root, reference))
            .map_err(internal)?;
        file.try_lock()
            .map_err(|_| blocked(reference, "operation_active"))?;
        Ok(Self(file))
    }
    fn active(root: &Path, reference: Uuid) -> Result<bool, MachineError> {
        let path = Self::path(root, reference);
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(internal(error)),
        };
        match file.try_lock() {
            Ok(()) => {
                file.unlock().map_err(internal)?;
                Ok(false)
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(true),
            Err(std::fs::TryLockError::Error(error)) => Err(internal(error)),
        }
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn owner(operation: &OneShotOperation) -> ReviewServerOwner {
    ReviewServerOwner {
        workspace_id: operation.input.workspace_id.clone(),
        request_ref: operation.input.request_ref,
    }
}

pub(crate) fn execute(
    workspace: Option<&Path>,
    profile: &str,
    invocation: Invocation,
    reference: Uuid,
    credential: &Path,
    temporary: bool,
) -> Result<Value, MachineError> {
    let request = invocation.request()?;
    let view = WorkspaceService::system()?.discover(workspace)?;
    let home = DolgoraeHome::system()?;
    let root = home.workspace_root(&view.workspace_id);
    let carrier = CredentialCarrier::open_path(credential)?;
    let recovery_controller = binding_from_carrier(&carrier, 1)?;
    let mut store = EngagementStore::open(&EngagementStore::workspace_database_path(&root))?;
    let _guard = OperationGuard::acquire(&root, reference)?;
    let input = OneShotOperationInput {
        request_ref: reference,
        workspace_id: view.workspace_id.clone(),
        request,
        profile: profile.to_owned(),
        recovery_controller,
        carrier_root: root
            .join("orchestration")
            .join(format!("one-shot-{reference}")),
        temporary_server: temporary,
    };
    let (_, created) = store.reserve_one_shot(&input)?;
    if !created {
        return Err(blocked(reference, "already_started"));
    }
    let outcome = (|| {
        let binding = ResolvedGlobalProfile::resolve(&home, profile)?.prepare(&home)?;
        store.bind_one_shot_profile(reference, &binding)?;
        let review_owner = ReviewServerOwner {
            workspace_id: view.workspace_id.clone(),
            request_ref: reference,
        };
        let prepared = crate::semantic::prepare_reviewer_with_binding(
            view,
            root.clone(),
            binding,
            Some(&review_owner),
        )?;
        let carriers = EphemeralCarriers::create_retained(input.carrier_root.clone())?;
        let capture_ref = if matches!(invocation, Invocation::Legacy(_)) {
            None
        } else {
            let capture = Uuid::now_v7();
            store.reserve_one_shot_capture(reference, capture)?;
            Some(capture)
        };
        let context = DurableReviewContext {
            prepared,
            carriers,
            capture_ref,
        };
        match invocation {
            Invocation::Legacy(request) => crate::review::execute_legacy_with_context(
                workspace,
                profile,
                &request,
                reference,
                Some(context),
            ),
            Invocation::Scoped(target, deadline) => {
                let mut request = ReviewRequest::fixed();
                request.deadline_seconds = deadline;
                crate::review::execute_scoped_common(
                    workspace,
                    profile,
                    &target,
                    reference,
                    &request,
                    None,
                    Some(context),
                )
            }
            Invocation::Task(request) => {
                let task = request.validate()?;
                let mut legacy = ReviewRequest::fixed();
                legacy.deadline_seconds = request.deadline_seconds;
                crate::review::execute_scoped_common(
                    workspace,
                    profile,
                    &request.target,
                    reference,
                    &legacy,
                    Some((&request, &task)),
                    Some(context),
                )
            }
        }
    })();
    let terminal = match &outcome {
        Ok(result) => OneShotTerminal::Succeeded {
            result: result.clone(),
        },
        Err(error) => OneShotTerminal::Failed {
            safe_error_code: error.code.clone(),
        },
    };
    store.finish_one_shot(reference, &terminal)?;
    let cleanup = cleanup_locked(&root, &mut store, reference);
    if outcome.is_ok() && cleanup.is_err() {
        return Err(blocked(reference, "cleanup_blocked"));
    }
    outcome
}

pub fn execute_cli(command: &str, arguments: &[OsString]) -> Result<Value, MachineError> {
    if required(arguments, "--format")? != "json" {
        return Err(MachineError::invalid_argument(
            "--format",
            "format must be json",
        ));
    }
    let reference = parse_reference(&required(arguments, "--request-ref")?)?;
    let workspace = crate::cli::option_path(arguments, "--workspace")
        .map_err(|reason| MachineError::invalid_argument("--workspace", reason))?;
    let view = WorkspaceService::system()?.discover(workspace.as_deref())?;
    let root = DolgoraeHome::system()?.workspace_root(&view.workspace_id);
    if command == "specialist.review-inspect" {
        return inspect(&root, &view, reference, false);
    }
    if required(arguments, "--action")? != "cleanup" {
        return Err(MachineError::invalid_argument(
            "--action",
            "action must be cleanup",
        ));
    }
    let path = crate::cli::option_path(arguments, "--recovery-controller-file")
        .map_err(|reason| MachineError::invalid_argument("--recovery-controller-file", reason))?
        .ok_or_else(|| {
            MachineError::invalid_argument(
                "--recovery-controller-file",
                "recovery credential is required",
            )
        })?;
    if !path.is_absolute() {
        return Err(MachineError::invalid_argument(
            "--recovery-controller-file",
            "recovery credential path must be absolute",
        ));
    }
    let database = EngagementStore::workspace_database_path(&root);
    if !matches!(database.try_exists(), Ok(true)) {
        return Err(blocked(reference, "operation_unknown"));
    }
    let readonly = EngagementStore::open_read_only(&database)?;
    let operation = readonly
        .one_shot(reference)?
        .ok_or_else(|| blocked(reference, "operation_unknown"))?;
    require_workspace(&operation, &view)?;
    let presented = CredentialCarrier::open_path(&path)
        .and_then(|carrier| binding_from_carrier(&carrier, 1))
        .map_err(|_| blocked(reference, "authority_unavailable"))?;
    if presented != operation.input.recovery_controller {
        return Err(blocked(reference, "authority_unavailable"));
    }
    drop(readonly);
    let _guard = OperationGuard::acquire(&root, reference)?;
    let mut store = EngagementStore::open(&database)?;
    cleanup_locked(&root, &mut store, reference).map_err(|error| {
        if error.code == "REVIEW_RECOVERY_BLOCKED" {
            error
        } else {
            blocked(reference, "cleanup_blocked")
        }
    })?;
    inspect(&root, &view, reference, true)
}

fn inspect(
    root: &Path,
    view: &WorkspaceView,
    reference: Uuid,
    owns_lock: bool,
) -> Result<Value, MachineError> {
    let mut result = json!({
        "schema":"dolgorae-one-shot-review-observation/v1", "request_ref":reference,
        "request_sha256":null,"profile":null,"observation":"unknown","outcome":"unknown",
        "result":null,"report":null,"safe_error_code":null,"diagnostic":null,"engagement":null,"reviewer":null,"task":null,"capture":null,"server":null,
        "recovery":{"status":"blocked","blocked_reasons":["operation_unknown"]}
    });
    let database = EngagementStore::workspace_database_path(root);
    if !database.try_exists().map_err(internal)? {
        return Ok(result);
    }
    let Ok(store) = EngagementStore::open_read_only(&database) else {
        return Ok(result);
    };
    let Ok(Some(observed)) = store.observe_one_shot(reference) else {
        return Ok(result);
    };
    let operation = &observed.operation;
    require_workspace(operation, view)?;
    let active = if owns_lock {
        false
    } else {
        let Ok(active) = OperationGuard::active(root, reference) else {
            return Ok(result);
        };
        active
    };
    result["request_sha256"] = json!(format!(
        "sha256:{}",
        operation.request_sha256.trim_start_matches("sha256:")
    ));
    result["profile"] = json!(operation.input.profile);
    result["observation"] = json!("known");
    let mut reasons = Vec::<&str>::new();
    if active {
        reasons.push("operation_active");
    }
    match &operation.terminal {
        Some(OneShotTerminal::Succeeded { result: original }) => {
            result["outcome"] = json!("succeeded");
            result["result"] = original.clone();
        }
        Some(OneShotTerminal::Failed { safe_error_code }) => {
            result["outcome"] = json!("failed");
            result["safe_error_code"] = json!(safe_error_code);
        }
        None => {
            result["outcome"] = json!(if reasons.contains(&"operation_active") {
                "pending"
            } else {
                "unknown"
            });
        }
    }
    let mut needed = false;
    if let Some(engagement) = &observed.engagement {
        result["engagement"] =
            json!({"engagement_id":engagement.engagement_id,"state":engagement.state});
        needed |= engagement.state != "closed";
        if let Some(run_id) = engagement.specialist_run_id {
            match RunStore::new(SystemWorkspacePlatform, root).load_state_projection(run_id) {
                Ok(state) => {
                    result["reviewer"] = json!({"run_id":run_id,"state":state.lifecycle.as_str()});
                    needed |= state.lifecycle != RunLifecycle::Closed;
                    match state.lifecycle {
                        RunLifecycle::Running | RunLifecycle::WaitingInteraction => {
                            reasons.push("reviewer_active")
                        }
                        RunLifecycle::Starting
                        | RunLifecycle::ReconciliationRequired
                        | RunLifecycle::OutcomeUnknown => reasons.push("reviewer_unknown"),
                        _ => {}
                    }
                }
                Err(_) => {
                    result["reviewer"] = json!({"run_id":run_id,"state":"unknown"});
                    reasons.push("reviewer_unknown");
                }
            }
        }
        if matches!(
            engagement.state.as_str(),
            "provisioning" | "recovery_required" | "interrupted_unknown" | "executing"
        ) {
            reasons.push("engagement_unknown");
        }
    }
    if let Some(task) = &observed.task {
        result["task"] = json!({"task_id":task.task_id,"state":task.state});
        result["diagnostic"] = json!(task.diagnostic);
        if task.safe_error_code.is_some() {
            result["safe_error_code"] = json!(task.safe_error_code);
        }
        if task.state == "failed" {
            result["outcome"] = json!("failed");
        }
        if matches!(
            task.state.as_str(),
            "accepted" | "running" | "interrupted_unknown"
        ) {
            reasons.push("outcome_unknown");
        }
    }
    if let Some(artifact) = &observed.artifact {
        result["report"] = json!({"artifact_id":artifact.artifact_id,"verdict":artifact.output});
    }
    if let Some(capture) = operation.capture_ref {
        result["capture"] = json!({"capture_ref":capture,"revision":null,"state":"reserved","cleanup_pending":false});
    }
    if let (Some(capture), Some(engagement)) = (operation.capture_ref, operation.engagement_id) {
        match crate::review_target::observe_capture_in_state_root(root, capture, engagement) {
            Ok(Some(capture)) => {
                needed |= capture["state"] != "settled" || capture["cleanup_pending"] == true;
                result["capture"] = capture;
            }
            Ok(None) => {
                if capture_publication_required(&observed) {
                    reasons.push("capture_unpublished");
                }
            }
            Err(_) => reasons.push("capture_invalid"),
        }
    }
    if let Some(binding) = &operation.global_profile_binding {
        match crate::profile::inspect_review_server(binding, &owner(operation)) {
            Ok(server) => {
                needed |= operation.input.temporary_server
                    && matches!(
                        server.status,
                        ReviewServerStatus::Owned | ReviewServerStatus::RecoveryBlocked
                    );
                if operation.input.temporary_server
                    && server.status == ReviewServerStatus::RecoveryBlocked
                {
                    reasons.push("server_unknown");
                }
                result["server"] = json!(server);
            }
            Err(_) => reasons.push("server_unknown"),
        }
    }
    let required_carriers = [
        (
            "aggregate.json",
            observed
                .engagement
                .as_ref()
                .is_some_and(|engagement| engagement.state != "closed"),
        ),
        (
            "reviewer.json",
            !result["reviewer"].is_null() && result["reviewer"]["state"] != "closed",
        ),
        (
            "settlement-owner",
            result["capture"]["state"] == "active" || result["capture"]["cleanup_pending"] == true,
        ),
        (
            "terminal-receipt.json",
            result["capture"]["cleanup_pending"] == true,
        ),
    ];
    if required_carriers
        .iter()
        .any(|(file, required)| *required && !operation.input.carrier_root.join(file).is_file())
    {
        reasons.push("authority_unavailable");
    }
    if operation.terminal.is_none() && observed.task.is_none() && reasons.is_empty() {
        reasons.push("outcome_unknown");
    }
    reasons.sort_unstable();
    reasons.dedup();
    result["recovery"] = json!({"status":if !reasons.is_empty() {"blocked"} else if needed {"available"} else {"not_needed"},"blocked_reasons":reasons});
    Ok(result)
}

fn cleanup_locked(
    root: &Path,
    store: &mut EngagementStore,
    reference: Uuid,
) -> Result<(), MachineError> {
    let observed = store
        .observe_one_shot(reference)?
        .ok_or_else(|| blocked(reference, "operation_unknown"))?;
    let operation = &observed.operation;
    let carriers = EphemeralCarriers::retained(operation.input.carrier_root.clone());
    if let Some(engagement) = &observed.engagement {
        if engagement.state != "closed" {
            let aggregate = CredentialCarrier::open_path(&carriers.aggregate)
                .and_then(|carrier| binding_from_carrier(&carrier, 1))
                .map_err(|_| blocked(reference, "authority_unavailable"))?;
            if aggregate.capability_sha256 != engagement.external_controller_ref_sha256 {
                return Err(blocked(reference, "authority_unavailable"));
            }
        }
        if matches!(
            engagement.state.as_str(),
            "provisioning" | "recovery_required" | "interrupted_unknown" | "executing"
        ) {
            return Err(blocked(reference, "cleanup_blocked"));
        }
        if let Some(task) = &observed.task
            && matches!(
                task.state.as_str(),
                "accepted" | "running" | "interrupted_unknown"
            )
        {
            return Err(blocked(reference, "cleanup_blocked"));
        }
        if let Some(run_id) = engagement.specialist_run_id {
            let run = RunStore::new(SystemWorkspacePlatform, root)
                .load_state_projection(run_id)
                .map_err(|_| blocked(reference, "cleanup_blocked"))?;
            if run.lifecycle != RunLifecycle::Closed {
                if !matches!(
                    run.lifecycle,
                    RunLifecycle::Idle | RunLifecycle::Paused | RunLifecycle::StartFailed
                ) {
                    return Err(blocked(reference, "cleanup_blocked"));
                }
                let manifest =
                    RunStore::new(SystemWorkspacePlatform, root).load_manifest(run_id)?;
                let workspace = manifest.canonical_workspace.to_path_buf()?;
                crate::review::close_reviewer(&workspace, &carriers.reviewer, run_id)
                    .map_err(|_| blocked(reference, "cleanup_blocked"))?;
            }
        }
        let key = |verb: &str| format!("one-shot-recovery:{reference}:{verb}");
        let current = store.snapshot(engagement.engagement_id)?;
        if matches!(current.state.as_str(), "open" | "ready") {
            store.cancel(engagement.engagement_id, &key("cancel"))?;
        }
        let current = store.snapshot(engagement.engagement_id)?;
        if matches!(
            current.state.as_str(),
            "cancelled" | "failed" | "result_ready"
        ) {
            store.release(engagement.engagement_id, &key("release"))?;
        }
        if store.snapshot(engagement.engagement_id)?.state == "released" {
            store.close(engagement.engagement_id, &key("close"))?;
        }
        if store.snapshot(engagement.engagement_id)?.state != "closed" {
            return Err(blocked(reference, "cleanup_blocked"));
        }
        if let Some(capture_ref) = operation.capture_ref {
            match crate::review_target::observe_capture_in_state_root(
                root,
                capture_ref,
                engagement.engagement_id,
            )? {
                Some(capture) if capture["state"] == "active" => {
                    let terminal = capture_terminal(
                        operation,
                        observed.task.as_ref(),
                        observed.artifact.is_some(),
                    )?;
                    let evidence = crate::jcs::sha256_hex(&serde_json::to_vec(&json!({"request_ref":reference,"engagement_id":engagement.engagement_id,"state":"closed","terminal":terminal})).map_err(internal)?);
                    crate::review::settle_review_target(
                        root,
                        capture_ref,
                        engagement.engagement_id,
                        terminal,
                        &evidence,
                        &carriers,
                    )?;
                }
                Some(capture)
                    if capture["state"] == "settled" && capture["cleanup_pending"] == true =>
                {
                    crate::review_target::settle_capture_in_state_root(
                        root,
                        capture_ref,
                        1,
                        &carriers.settlement_owner,
                        &carriers.terminal_receipt,
                    )?;
                }
                None if capture_publication_required(&observed) => {
                    return Err(blocked(reference, "cleanup_blocked"));
                }
                _ => {}
            }
        }
    }
    if operation.input.temporary_server
        && let Some(binding) = &operation.global_profile_binding
    {
        let status = crate::profile::retire_review_server(binding, &owner(operation))?;
        if !matches!(
            status.status,
            ReviewServerStatus::Retired
                | ReviewServerStatus::Preexisting
                | ReviewServerStatus::NotStarted
        ) {
            return Err(blocked(reference, "cleanup_blocked"));
        }
    }
    Ok(())
}

fn capture_publication_required(observed: &crate::engagement::OneShotObservation) -> bool {
    observed.operation.terminal.is_none()
        || matches!(
            observed.operation.terminal,
            Some(OneShotTerminal::Succeeded { .. })
        )
        || observed.task.is_some()
        || observed
            .engagement
            .as_ref()
            .is_some_and(|engagement| engagement.specialist_run_id.is_some())
}

fn capture_terminal(
    operation: &OneShotOperation,
    task: Option<&crate::engagement::OneShotTaskSnapshot>,
    artifact: bool,
) -> Result<&'static str, MachineError> {
    match &operation.terminal {
        Some(OneShotTerminal::Succeeded { .. }) => Ok("completed"),
        Some(OneShotTerminal::Failed { .. }) => Ok("failed"),
        None if artifact
            && task.is_some_and(|task| {
                matches!(task.state.as_str(), "result_ready" | "delivered")
            }) =>
        {
            Ok("completed")
        }
        None if task.is_some_and(|task| matches!(task.state.as_str(), "failed" | "cancelled")) => {
            Ok("failed")
        }
        None => Err(blocked(operation.input.request_ref, "cleanup_blocked")),
    }
}

fn require_workspace(
    operation: &OneShotOperation,
    view: &WorkspaceView,
) -> Result<(), MachineError> {
    if operation.input.workspace_id != view.workspace_id {
        return Err(blocked(operation.input.request_ref, "operation_unknown"));
    }
    Ok(())
}
pub(crate) fn parse_reference(value: &str) -> Result<Uuid, MachineError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|value| value.get_version_num() == 7)
        .ok_or_else(|| MachineError::invalid_argument("--request-ref", "reference must be UUIDv7"))
}
fn required(arguments: &[OsString], flag: &str) -> Result<String, MachineError> {
    crate::cli::option_path(arguments, flag)
        .map_err(|reason| MachineError::invalid_argument(flag, reason))?
        .and_then(|value| value.into_os_string().into_string().ok())
        .ok_or_else(|| MachineError::invalid_argument(flag, "required UTF-8 value is missing"))
}
fn blocked(reference: Uuid, reason: &str) -> MachineError {
    MachineError::new(
        "REVIEW_RECOVERY_BLOCKED",
        "the original review requires inspection before further recovery",
        false,
        json!({"request_ref":reference,"reason":reason,"required_action":"inspect_original_operation"}),
    )
}
fn internal(_error: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "one-shot operation storage is unavailable",
        false,
        json!({"reason":"one-shot operation storage is unavailable"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ControllerIdentity, ControllerKind};
    use crate::engagement::OneShotTaskSnapshot;
    use crate::run::ControllerBinding;
    use crate::workspace::{LosslessPath, WorkspaceMode};
    use std::fs;

    fn fixture(root: &Path) -> (WorkspaceView, OneShotOperationInput) {
        let view = WorkspaceView {
            workspace_id: "recovery-test".to_owned(),
            canonical_path: LosslessPath::from_path(root),
            mode: WorkspaceMode::NonGit,
            created: false,
        };
        let input = OneShotOperationInput {
            request_ref: Uuid::now_v7(),
            workspace_id: view.workspace_id.clone(),
            request: json!({"version":1,"deadline_seconds":60}),
            profile: "reviewer".to_owned(),
            recovery_controller: ControllerBinding {
                identity: ControllerIdentity {
                    controller_id: Uuid::now_v7(),
                    kind: ControllerKind::Automation,
                    instance_id: "recovery-test".to_owned(),
                    subject_id: None,
                    generation: 1,
                },
                capability_sha256: "a".repeat(64),
            },
            carrier_root: root.join("carriers"),
            temporary_server: false,
        };
        (view, input)
    }

    #[test]
    fn inspection_never_creates_unknown_storage_or_replays_accepted_work() {
        let root = std::env::temp_dir().join(format!("dolgorae-recovery-{}", Uuid::now_v7()));
        let (view, input) = fixture(&root);
        let unknown = inspect(&root, &view, input.request_ref, false).unwrap();
        assert_eq!(unknown["observation"], "unknown");
        assert!(!root.exists());
        let mut store =
            EngagementStore::open(&EngagementStore::workspace_database_path(&root)).unwrap();
        store.reserve_one_shot(&input).unwrap();
        let capture = Uuid::now_v7();
        store
            .reserve_one_shot_capture(input.request_ref, capture)
            .unwrap();
        let before = store.one_shot(input.request_ref).unwrap();
        let guard = OperationGuard::acquire(&root, input.request_ref).unwrap();
        assert!(OperationGuard::acquire(&root, input.request_ref).is_err());
        let active = inspect(&root, &view, input.request_ref, false).unwrap();
        assert_eq!(active["outcome"], "pending");
        assert_eq!(active["capture"]["state"], "reserved");
        assert_eq!(active["capture"]["capture_ref"], capture.to_string());
        assert!(active["engagement"].is_null());
        drop(guard);
        let lost = inspect(&root, &view, input.request_ref, false).unwrap();
        assert_eq!(lost["outcome"], "unknown");
        assert_eq!(lost["recovery"]["status"], "blocked");
        assert_eq!(store.one_shot(input.request_ref).unwrap(), before);
        let lock_path = OperationGuard::path(&root, input.request_ref);
        fs::remove_file(&lock_path).unwrap();
        let unrelated = root.join("unrelated");
        fs::write(&unrelated, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&unrelated, &lock_path).unwrap();
        let unreadable_lock = inspect(&root, &view, input.request_ref, false).unwrap();
        assert_eq!(unreadable_lock["observation"], "unknown");
        assert_eq!(unreadable_lock["recovery"]["status"], "blocked");
        assert_eq!(fs::read(&unrelated).unwrap(), b"unchanged");
        assert!(OperationGuard::acquire(&root, input.request_ref).is_err());
        fs::remove_file(lock_path).unwrap();
        let connection =
            rusqlite::Connection::open(EngagementStore::workspace_database_path(&root)).unwrap();
        connection
            .execute(
                "UPDATE one_shot_operations SET receipt_sha256='corrupt'",
                [],
            )
            .unwrap();
        let corrupt = inspect(&root, &view, input.request_ref, false).unwrap();
        assert_eq!(corrupt["observation"], "unknown");
        assert_eq!(corrupt["recovery"]["status"], "blocked");
        assert!(corrupt["engagement"].is_null());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn response_loss_uses_terminal_task_evidence_without_inventing_failure() {
        let root = std::env::temp_dir().join(format!("dolgorae-recovery-{}", Uuid::now_v7()));
        let (_, input) = fixture(&root);
        let mut store =
            EngagementStore::open(&EngagementStore::workspace_database_path(&root)).unwrap();
        let (operation, _) = store.reserve_one_shot(&input).unwrap();
        let mut task = OneShotTaskSnapshot {
            task_id: Uuid::now_v7(),
            specialist_run_id: Uuid::now_v7(),
            state: "result_ready".to_owned(),
            safe_error_code: None,
            diagnostic: None,
        };
        for state in ["result_ready", "delivered"] {
            task.state = state.to_owned();
            assert_eq!(
                capture_terminal(&operation, Some(&task), true).unwrap(),
                "completed"
            );
            assert!(capture_terminal(&operation, Some(&task), false).is_err());
        }
        assert!(capture_terminal(&operation, None, false).is_err());
        task.state = "running".to_owned();
        assert!(capture_terminal(&operation, Some(&task), true).is_err());
        task.state = "failed".to_owned();
        assert_eq!(
            capture_terminal(&operation, Some(&task), false).unwrap(),
            "failed"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
