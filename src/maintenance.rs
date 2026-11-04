//! Operator-owned workspace checkpoint maintenance; no implicit retention GC.
use crate::audit::AuditKind;
use crate::controller::OperatorAuthorization;
use crate::darwin::DarwinSystem;
use crate::domain::RunLifecycle;
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::ledger::{LedgerError, ObservedLedger, RunStateProjection};
use crate::ledger_generation::{self, PinnedGeneration, SwitchBarrier, WorkspaceLease};
use crate::machine::MachineError;
use crate::run::RunStore;
use crate::workspace::{
    SystemWorkspacePlatform, sync_directory, verify_secure_directory, verify_secure_file,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_CONTROL_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePlan {
    pub plan_ref: Uuid,
    pub workspace_ref: String,
    pub selected_generation: u64,
    pub dry_run: bool,
    pub kind: String,
    pub admission_closed: bool,
    pub active_writers: u64,
    pub unknown_workload: u64,
    pub protected_reference_count: u64,
    pub retire_content: Vec<Value>,
    pub retained_receipt_count: u64,
    pub expected_manifest_digest: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunBasis {
    run_id: Uuid,
    manifest_sha256: String,
    ledger_generation: u64,
    head: crate::ledger::LedgerHead,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedOperation {
    schema: String,
    plan: MaintenancePlan,
    operator_id: Uuid,
    operator_generation: u64,
    runs: Vec<RunBasis>,
    state: String,
    completed_runs: Vec<Uuid>,
    selected_manifest_digest: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintenanceOwner {
    schema: String,
    operation_ref: Uuid,
    plan_sha256: String,
    operator_id: Uuid,
    operator_generation: u64,
    workspace_ref: String,
    source_generation: u64,
    runs_sha256: String,
}
fn expected_owner(saved: &SavedOperation) -> Result<MaintenanceOwner, MachineError> {
    Ok(MaintenanceOwner {
        schema: "dolgorae.native-maintenance-owner/v1".to_owned(),
        operation_ref: saved.plan.plan_ref,
        plan_sha256: digest(&saved.plan)?,
        operator_id: saved.operator_id,
        operator_generation: saved.operator_generation,
        workspace_ref: saved.plan.workspace_ref.clone(),
        source_generation: saved.plan.selected_generation,
        runs_sha256: digest(&saved.runs)?,
    })
}
fn retained_owner(root: &Path, saved: &SavedOperation) -> Result<bool, MachineError> {
    let path = root.join(format!("{}.owner.json", saved.plan.plan_ref));
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if saved.state != "planned" {
                return Err(error(
                    "LOOKUP_NAMESPACE_UNAVAILABLE",
                    "maintenance owner binding is missing",
                ));
            }
            Ok(false)
        }
        Err(_) => Err(error(
            "LOOKUP_NAMESPACE_UNAVAILABLE",
            "maintenance owner binding unavailable",
        )),
        Ok(_) => {
            let owner: MaintenanceOwner = read_json(&path)?;
            if owner != expected_owner(saved)? {
                return Err(error(
                    "LOOKUP_NAMESPACE_CORRUPT",
                    "maintenance owner binding is corrupt",
                ));
            }
            Ok(true)
        }
    }
}
fn retain_owner(root: &Path, saved: &SavedOperation) -> Result<(), MachineError> {
    if retained_owner(root, saved)? {
        return Ok(());
    }
    let path = root.join(format!("{}.owner.json", saved.plan.plan_ref));
    let temporary = root.join(format!(".owner-{}.tmp", Uuid::now_v7()));
    let bytes = serde_json::to_vec(&expected_owner(saved)?).map_err(|_| {
        error(
            "RECOVERY_REQUIRED",
            "maintenance owner serialization failed",
        )
    })?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::hard_link(&temporary, &path)?;
        sync_directory(root)?;
        Ok::<_, std::io::Error>(())
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|_| {
        error(
            "RECOVERY_REQUIRED",
            "maintenance owner retention is uncertain",
        )
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceSelection {
    schema: String,
    generation: u64,
    operation_ref: Uuid,
    runs: Vec<RunBasis>,
}

fn error(code: &str, reason: impl Into<String>) -> MachineError {
    MachineError::new(
        code,
        reason.into(),
        false,
        json!({"required_action":"observe_original_maintenance"}),
    )
}
fn ledger_error(error_value: LedgerError) -> MachineError {
    let code = match &error_value {
        LedgerError::Capacity(_) => "STORAGE_CAPACITY_EXCEEDED",
        LedgerError::WriterBusy(_) => "RUN_BUSY",
        LedgerError::Integrity(_) => "ARTIFACT_INTEGRITY_FAILURE",
        _ => "RECOVERY_REQUIRED",
    };
    error(code, error_value.to_string())
}
fn digest(value: &impl Serialize) -> Result<String, MachineError> {
    let bytes = serde_json::to_vec(value).map_err(|e| error("RECOVERY_REQUIRED", e.to_string()))?;
    let value =
        parse(std::str::from_utf8(&bytes).map_err(|e| error("RECOVERY_REQUIRED", e.to_string()))?)
            .map_err(|e| error("RECOVERY_REQUIRED", e.to_string()))?;
    Ok(format!(
        "sha256:{}",
        sha256_hex(&canonicalize(&value).map_err(|e| error("RECOVERY_REQUIRED", e.to_string()))?)
    ))
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, MachineError> {
    read_json_at(path, true)
}
fn read_json_at<T: serde::de::DeserializeOwned>(
    path: &Path,
    retained: bool,
) -> Result<T, MachineError> {
    let invalid = || {
        if retained {
            error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "retained maintenance metadata is untrusted",
            )
        } else {
            MachineError::invalid_argument("--plan-file", "complete bounded plan JSON required")
        }
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| {
            if retained {
                error(
                    "LOOKUP_NAMESPACE_UNAVAILABLE",
                    "retained maintenance namespace unavailable",
                )
            } else {
                MachineError::runtime_path_invalid(path, e.to_string())
            }
        })?;
    verify_secure_file(path, DarwinSystem.current_uid())
        .map_err(|e| if retained { invalid() } else { e })?;
    if file.metadata().map_err(|_| invalid())?.len() > MAX_CONTROL_BYTES {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONTROL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > MAX_CONTROL_BYTES {
        return Err(invalid());
    }
    parse(std::str::from_utf8(&bytes).map_err(|_| invalid())?).map_err(|_| invalid())?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

fn trusted_original(
    root: &Path,
    workspace: &str,
    id: Uuid,
    generation: u64,
    operator: &OperatorAuthorization,
    plan: Option<&MaintenancePlan>,
) -> bool {
    if id.get_version_num() != 7 {
        return false;
    }
    if let Ok(saved) = load_operation(root, workspace, id, operator)
        && saved.plan.selected_generation == generation
        && plan.is_none_or(|p| saved.plan == *p)
        && (saved.state != "planned" || retained_owner(root, &saved).unwrap_or(false))
    {
        return true;
    }
    let Ok(owner) = read_json::<MaintenanceOwner>(&root.join(format!("{id}.owner.json"))) else {
        return false;
    };
    let digest_valid = |value: &str| {
        value.strip_prefix("sha256:").is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    };
    owner.schema == "dolgorae.native-maintenance-owner/v1"
        && owner.operation_ref == id
        && owner.operator_id == operator.operator_id
        && owner.operator_generation > 0
        && owner.workspace_ref == workspace
        && owner.source_generation == generation
        && digest_valid(&owner.plan_sha256)
        && digest_valid(&owner.runs_sha256)
        && plan.is_none_or(|p| {
            digest(p).is_ok_and(|d| d == owner.plan_sha256)
                && p.expected_manifest_digest == owner.runs_sha256
        })
}
fn namespace_failure(mut failure: MachineError, original: Option<Uuid>) -> MachineError {
    if matches!(
        failure.code.as_str(),
        "LOOKUP_NAMESPACE_CORRUPT" | "LOOKUP_NAMESPACE_UNAVAILABLE" | "ARTIFACT_INTEGRITY_FAILURE"
    ) {
        failure.details = json!({"operation_ref":original,"effect":"unresolved","required_action":"restore_state"});
    }
    failure
}

fn persist(path: &Path, value: &impl Serialize) -> Result<(), MachineError> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        error(
            "RECOVERY_REQUIRED",
            "maintenance state serialization failed",
        )
    })?;
    if bytes.len() as u64 > MAX_CONTROL_BYTES {
        return Err(error(
            "STORAGE_CAPACITY_EXCEEDED",
            "maintenance control namespace limit reached",
        ));
    }
    let temporary = path.with_extension(format!("{}.tmp", Uuid::now_v7()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_directory(path.parent().expect("state path parent"))?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|_| {
        error(
            "RECOVERY_REQUIRED",
            "maintenance state durability is uncertain",
        )
    })
}
fn control_root(state_root: &Path) -> Result<PathBuf, MachineError> {
    control_root_named(state_root, "maintenance")
}
fn control_root_named(state_root: &Path, name: &str) -> Result<PathBuf, MachineError> {
    verify_secure_directory(state_root, DarwinSystem.current_uid())?;
    let root = state_root.join(name);
    match fs::create_dir(&root) {
        Ok(()) => {
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(|_| error("RECOVERY_REQUIRED", "maintenance namespace creation failed"))?;
            sync_directory(state_root)
                .map_err(|_| error("RECOVERY_REQUIRED", "maintenance namespace sync failed"))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(error(
                "RECOVERY_REQUIRED",
                "maintenance namespace unavailable",
            ));
        }
    }
    verify_secure_directory(&root, DarwinSystem.current_uid())?;
    Ok(root)
}
fn inspect_namespace(root: &Path, except: Option<Uuid>) -> Result<(bool, u64), MachineError> {
    let content = content_pending(
        root.parent()
            .ok_or_else(|| error("RECOVERY_REQUIRED", "maintenance workspace absent"))?,
        except,
    )?;
    let (checkpoint, generation) = inspect_checkpoint_namespace(root, except)?;
    Ok((content || checkpoint, generation))
}
fn inspect_checkpoint_namespace(
    root: &Path,
    except: Option<Uuid>,
) -> Result<(bool, u64), MachineError> {
    let mut pending = false;
    let mut completed_generations = std::collections::BTreeMap::new();
    let mut count = 0;
    for entry in fs::read_dir(root)
        .map_err(|_| error("RECOVERY_REQUIRED", "maintenance namespace unavailable"))?
    {
        let entry =
            entry.map_err(|_| error("RECOVERY_REQUIRED", "maintenance namespace unreadable"))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| error("RECOVERY_REQUIRED", "maintenance namespace name invalid"))?;
        if name == "selected.json" || name.ends_with(".tmp") {
            continue;
        }
        if let Some(id) = name
            .strip_suffix(".owner.json")
            .and_then(|name| Uuid::parse_str(name).ok())
        {
            let saved: SavedOperation = read_json(&root.join(format!("{id}.json")))?;
            if !retained_owner(root, &saved)? {
                return Err(error(
                    "RECOVERY_REQUIRED",
                    "maintenance owner namespace is incomplete",
                ));
            }
            continue;
        }
        let id = name
            .strip_suffix(".json")
            .and_then(|name| Uuid::parse_str(name).ok())
            .filter(|id| id.get_version_num() == 7)
            .ok_or_else(|| {
                error(
                    "RECOVERY_REQUIRED",
                    "maintenance namespace identity invalid",
                )
            })?;
        let saved: SavedOperation = read_json(&entry.path())?;
        if saved.schema != "dolgorae.native-maintenance-operation/v1"
            || saved.plan.plan_ref != id
            || saved.plan.expected_manifest_digest != digest(&saved.runs)?
            || !matches!(saved.state.as_str(), "planned" | "preparing" | "completed")
        {
            return Err(error(
                "RECOVERY_REQUIRED",
                "maintenance namespace integrity is invalid",
            ));
        }
        count += 1;
        if count > 100_000 {
            return Err(error(
                "STORAGE_CAPACITY_EXCEEDED",
                "maintenance namespace observation bound reached",
            ));
        }
        let owned = retained_owner(root, &saved)?;
        pending |= owned && saved.state != "completed" && Some(id) != except;
        if saved.state == "completed" {
            let generation = saved
                .plan
                .selected_generation
                .checked_add(1)
                .ok_or_else(|| error("RECOVERY_REQUIRED", "maintenance generation overflow"))?;
            if completed_generations.insert(generation, id).is_some() {
                return Err(error(
                    "RECOVERY_REQUIRED",
                    "maintenance generation has duplicate completion owners",
                ));
            }
        }
    }
    Ok((
        pending,
        completed_generations
            .keys()
            .next_back()
            .copied()
            .unwrap_or(1),
    ))
}

fn workspace_generation(root: &Path) -> Result<u64, MachineError> {
    let path = root.join("selected.json");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if inspect_namespace(root, None)?.1 > 1 {
                return Err(error(
                    "RECOVERY_REQUIRED",
                    "completed maintenance has lost workspace selection",
                ));
            }
            Ok(1)
        }
        Err(_) => Err(error(
            "RECOVERY_REQUIRED",
            "workspace generation unavailable",
        )),
        Ok(_) => {
            let selected: WorkspaceSelection = read_json(&path)?;
            if selected.schema != "dolgorae.native-maintenance-selection/v1"
                || selected.generation < 2
                || selected.operation_ref.get_version_num() != 7
            {
                return Err(error("RECOVERY_REQUIRED", "workspace generation invalid"));
            }
            let completed_generation = inspect_namespace(root, None)?.1;
            let saved: SavedOperation =
                read_json(&root.join(format!("{}.json", selected.operation_ref)))?;
            let expected_generation = saved
                .plan
                .selected_generation
                .checked_add(1)
                .ok_or_else(|| error("RECOVERY_REQUIRED", "maintenance generation overflow"))?;
            let mut expected_runs = saved.runs.clone();
            for basis in &mut expected_runs {
                basis.ledger_generation = basis
                    .ledger_generation
                    .checked_add(1)
                    .ok_or_else(|| error("RECOVERY_REQUIRED", "ledger generation overflow"))?;
            }
            let allowed_generation = if saved.state == "completed" {
                completed_generation
            } else if saved.state == "preparing" {
                completed_generation
                    .checked_add(1)
                    .ok_or_else(|| error("RECOVERY_REQUIRED", "maintenance generation overflow"))?
            } else {
                return Err(error(
                    "RECOVERY_REQUIRED",
                    "workspace selector lacks an applied owner",
                ));
            };
            if !retained_owner(root, &saved)?
                || selected.operation_ref != saved.plan.plan_ref
                || selected.generation != expected_generation
                || selected.generation != allowed_generation
                || selected.runs != expected_runs
                || saved.completed_runs.len() != saved.runs.len()
                || (saved.state == "completed"
                    && digest(&selected.runs)? != saved.selected_manifest_digest)
            {
                return Err(error(
                    "RECOVERY_REQUIRED",
                    "workspace selection disagrees with retained completion or original owner",
                ));
            }
            Ok(selected.generation)
        }
    }
}
fn run_ids(state_root: &Path) -> Result<Vec<Uuid>, MachineError> {
    run_ids_with_bootstrap(state_root, false)
}
fn run_ids_with_bootstrap(
    state_root: &Path,
    allow_bootstrap: bool,
) -> Result<Vec<Uuid>, MachineError> {
    verify_secure_directory(&state_root.join("runs"), DarwinSystem.current_uid())?;
    let mut runs = Vec::new();
    for entry in fs::read_dir(state_root.join("runs"))
        .map_err(|_| error("RECOVERY_REQUIRED", "Run namespace unavailable"))?
    {
        let entry = entry.map_err(|_| error("RECOVERY_REQUIRED", "Run namespace unreadable"))?;
        let name = entry.file_name();
        // RunStore owns temporary bootstrap directories; they are not accepted Runs.
        if name.to_str().is_some_and(|name| name.starts_with('.')) {
            if allow_bootstrap {
                continue;
            }
            return Err(error("RUN_BUSY", "Run bootstrap is in progress"));
        }
        let run = name
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
            .filter(|id| id.get_version_num() == 7)
            .ok_or_else(|| {
                error(
                    "RECOVERY_REQUIRED",
                    "Run namespace contains an invalid identity",
                )
            })?;
        verify_secure_directory(&entry.path(), DarwinSystem.current_uid())?;
        runs.push(run);
        if runs.len() > 4096 {
            return Err(error(
                "STORAGE_CAPACITY_EXCEEDED",
                "workspace maintenance Run reference bound reached",
            ));
        }
    }
    runs.sort();
    Ok(runs)
}
fn observe_basis(
    state_root: &Path,
    workspace_id: &str,
    run_id: Uuid,
) -> Result<
    (
        RunBasis,
        ObservedLedger,
        RunStateProjection,
        PinnedGeneration,
    ),
    MachineError,
> {
    let root = state_root.join("runs").join(run_id.to_string());
    let manifest = RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(run_id)?;
    if manifest.workspace_id != workspace_id {
        return Err(error(
            "RECOVERY_REQUIRED",
            "Run belongs to another workspace",
        ));
    }
    let projection: RunStateProjection = read_json(&root.join("state.json"))?;
    let ledger = ObservedLedger::open_complete(&root, run_id, projection.ledger_head.sequence)
        .map_err(ledger_error)?;
    let replayed = crate::ledger::project(run_id, ledger.records()).map_err(ledger_error)?;
    if replayed != projection {
        return Err(error(
            "RECOVERY_REQUIRED",
            "Run state disagrees with complete retained history",
        ));
    }
    crate::conformance::verify_ledger_records(run_id, ledger.records())
        .map_err(|e| error("RECOVERY_REQUIRED", e.to_string()))?;
    let pin = ledger_generation::pin(&root, run_id).map_err(ledger_error)?;
    ledger_generation::validate_checkpoint(&root, &pin, ledger.records()).map_err(ledger_error)?;
    let basis = RunBasis {
        run_id,
        manifest_sha256: digest(&manifest)?,
        ledger_generation: pin.manifest.as_ref().map_or(1, |m| m.generation),
        head: projection.ledger_head.clone(),
    };
    Ok((basis, ledger, projection, pin))
}
fn workload_counts(state_root: &Path, run_id: Uuid) -> Result<(u64, u64), MachineError> {
    let path = crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run_id)
        .map_err(|e| error("RECOVERY_REQUIRED", format!("{e:?}")))?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((0, 0)),
        Err(_) => Err(error("RECOVERY_REQUIRED", "worker identity unavailable")),
        Ok(_) => {
            let record = crate::worker::read_runtime_record(&path, DarwinSystem.current_uid())
                .map_err(|e| error("RECOVERY_REQUIRED", format!("{e:?}")))?;
            match crate::worker::classify_worker_identity(&record) {
                crate::worker::ProcessIdentityVerdict::Match => Ok((1, 0)),
                crate::worker::ProcessIdentityVerdict::Absent => Ok((
                    0,
                    u64::from(crate::worker::prove_worker_generation_absent(&record).is_err()),
                )),
                _ => Ok((0, 1)),
            }
        }
    }
}

pub fn plan_checkpoint(
    state_root: &Path,
    workspace_id: &str,
    selected_generation: u64,
    operator: &OperatorAuthorization,
) -> Result<MaintenancePlan, MachineError> {
    let root = control_root(state_root)?;
    if inspect_namespace(&root, None)?.0 {
        return Err(error(
            "RECOVERY_REQUIRED",
            "resume the retained maintenance owner before creating another plan",
        ));
    }
    let generation = workspace_generation(&root)?;
    if generation != selected_generation {
        return Err(error(
            "MAINTENANCE_PLAN_STALE",
            "selected workspace generation changed",
        ));
    }
    let mut bases = Vec::new();
    let mut active_writers = 0;
    let mut unknown_workload = 0;
    let mut receipts = 0;
    for id in run_ids(state_root)? {
        let (basis, ledger, projection, _) = observe_basis(state_root, workspace_id, id)?;
        let (active, unknown) = workload_counts(state_root, id)?;
        active_writers += active;
        unknown_workload +=
            unknown + u64::from(matches!(projection.lifecycle, RunLifecycle::OutcomeUnknown));
        receipts += ledger
            .records()
            .iter()
            .filter(|r| {
                matches!(
                    r.kind(),
                    AuditKind::TurnStarted | AuditKind::MutationCompleted
                )
            })
            .count() as u64;
        bases.push(basis);
    }
    let plan = MaintenancePlan {
        plan_ref: Uuid::now_v7(),
        workspace_ref: workspace_id.to_owned(),
        selected_generation,
        dry_run: true,
        kind: "ledger_checkpoint".to_owned(),
        admission_closed: false,
        active_writers,
        unknown_workload,
        protected_reference_count: bases.len() as u64,
        retire_content: Vec::new(),
        retained_receipt_count: receipts,
        expected_manifest_digest: digest(&bases)?,
    };
    let saved = SavedOperation {
        schema: "dolgorae.native-maintenance-operation/v1".to_owned(),
        plan: plan.clone(),
        operator_id: operator.operator_id,
        operator_generation: operator.operator_generation,
        runs: bases,
        state: "planned".to_owned(),
        completed_runs: Vec::new(),
        selected_manifest_digest: plan.expected_manifest_digest.clone(),
    };
    let path = root.join(format!("{}.json", plan.plan_ref));
    if path.exists() {
        return Err(error(
            "RECOVERY_REQUIRED",
            "maintenance plan identity collision",
        ));
    }
    persist(&path, &saved)?;
    Ok(plan)
}
fn load_operation(
    root: &Path,
    workspace_id: &str,
    plan_id: Uuid,
    operator: &OperatorAuthorization,
) -> Result<SavedOperation, MachineError> {
    if plan_id.get_version_num() != 7 {
        return Err(error("INVALID_ARGUMENT", "maintenance ID must be UUIDv7"));
    }
    let saved: SavedOperation = read_json(&root.join(format!("{plan_id}.json")))?;
    if saved.schema != "dolgorae.native-maintenance-operation/v1"
        || saved.plan.plan_ref != plan_id
        || saved.plan.workspace_ref != workspace_id
        || saved.operator_id != operator.operator_id
        || saved.plan.expected_manifest_digest != digest(&saved.runs)?
        || !saved.plan.dry_run
        || saved.plan.kind != "ledger_checkpoint"
        || !saved.plan.retire_content.is_empty()
        || saved.plan.selected_generation == 0
        || saved.operator_generation == 0
        || saved
            .runs
            .windows(2)
            .any(|pair| pair[0].run_id >= pair[1].run_id)
        || saved.runs.len() > 4096
        || saved.completed_runs.len() > saved.runs.len()
        || saved
            .completed_runs
            .iter()
            .any(|id| !saved.runs.iter().any(|basis| basis.run_id == *id))
        || saved
            .completed_runs
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != saved.completed_runs.len()
        || (saved.state == "planned" && !saved.completed_runs.is_empty())
        || (saved.state == "completed" && saved.completed_runs.len() != saved.runs.len())
        || !matches!(saved.state.as_str(), "planned" | "preparing" | "completed")
    {
        return Err(error(
            "LOOKUP_NAMESPACE_CORRUPT",
            "maintenance original scope or namespace integrity is invalid",
        ));
    }
    Ok(saved)
}
fn observation(saved: &SavedOperation) -> Value {
    json!({"operation_ref":saved.plan.plan_ref,"plan":saved.plan,"state":if saved.state == "preparing" {"recovery_needed"} else {&saved.state},
        "old_generation":saved.plan.selected_generation,"new_generation":if saved.state == "completed" {Some(saved.plan.selected_generation+1)} else {None},
        "selected_manifest_digest":saved.selected_manifest_digest,
        "required_action":if saved.state == "preparing" {"resume_maintenance"} else {"none"}})
}
pub fn get_checkpoint(
    state_root: &Path,
    workspace_id: &str,
    plan_id: Uuid,
    selected_generation: u64,
    operator: &OperatorAuthorization,
) -> Result<Value, MachineError> {
    let root = state_root.join("maintenance");
    let known = trusted_original(
        &root,
        workspace_id,
        plan_id,
        selected_generation,
        operator,
        None,
    );
    let result = (|| {
        verify_secure_directory(&root, DarwinSystem.current_uid()).map_err(|_| {
            error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "retained maintenance directory untrusted",
            )
        })?;
        let mut saved = load_operation(&root, workspace_id, plan_id, operator)?;
        if retained_owner(&root, &saved)? && saved.state == "planned" {
            saved.state = "preparing".to_owned();
        }
        if saved.plan.selected_generation != selected_generation {
            return Err(error(
                "INVALID_ARGUMENT",
                "observation requires original selected generation",
            ));
        }
        Ok(observation(&saved))
    })();
    result.map_err(|e| namespace_failure(e, known.then_some(plan_id)))
}
pub fn apply_checkpoint(
    state_root: &Path,
    workspace_id: &str,
    plan: &MaintenancePlan,
    explicit_approval: bool,
    operator: &OperatorAuthorization,
) -> Result<Value, MachineError> {
    apply_checkpoint_with_faults(
        state_root,
        workspace_id,
        plan,
        explicit_approval,
        operator,
        &crate::fault::NoFaults,
    )
}
pub fn apply_checkpoint_with_faults(
    state_root: &Path,
    workspace_id: &str,
    plan: &MaintenancePlan,
    explicit_approval: bool,
    operator: &OperatorAuthorization,
    faults: &dyn crate::fault::FaultInjector,
) -> Result<Value, MachineError> {
    let root = state_root.join("maintenance");
    let known = trusted_original(
        &root,
        workspace_id,
        plan.plan_ref,
        plan.selected_generation,
        operator,
        Some(plan),
    );
    let result = apply_checkpoint_with(
        state_root,
        workspace_id,
        plan,
        explicit_approval,
        operator,
        &mut |barrier| {
            let fault = match barrier {
                SwitchBarrier::OwnerRetained => {
                    crate::fault::FaultBarrier::AfterMaintenanceOwnerSync
                }
                SwitchBarrier::SegmentSynced => {
                    crate::fault::FaultBarrier::AfterMaintenanceSegmentSync
                }
                SwitchBarrier::CheckpointSynced => {
                    crate::fault::FaultBarrier::AfterMaintenanceCheckpointSync
                }
                SwitchBarrier::IndexSynced => {
                    crate::fault::FaultBarrier::AfterMaintenanceReceiptIndexSync
                }
                SwitchBarrier::ManifestSynced => {
                    crate::fault::FaultBarrier::AfterMaintenanceManifestSync
                }
                SwitchBarrier::DirectorySynced => {
                    crate::fault::FaultBarrier::AfterMaintenanceDirectorySync
                }
                SwitchBarrier::Selected => {
                    crate::fault::FaultBarrier::AfterMaintenanceSelectionReplace
                }
                SwitchBarrier::SelectionSynced => {
                    crate::fault::FaultBarrier::AfterMaintenanceSelectionSync
                }
                SwitchBarrier::WorkspaceSelected => {
                    crate::fault::FaultBarrier::AfterMaintenanceWorkspaceSelectionSync
                }
                SwitchBarrier::Completed => {
                    crate::fault::FaultBarrier::AfterMaintenanceCompletionSync
                }
            };
            faults.check(fault).map_err(LedgerError::Fault)
        },
    );
    result.map_err(|mut failure| {
        let original = (known || trusted_original(&root, workspace_id, plan.plan_ref,
            plan.selected_generation, operator, Some(plan))).then_some(plan.plan_ref);
        if original.is_some() && failure.code == "RECOVERY_REQUIRED" {
            failure.details = json!({"operation_ref":original,"effect":"unresolved","required_action":"resume_maintenance"});
        }
        namespace_failure(failure, original)
    })
}

fn apply_checkpoint_with(
    state_root: &Path,
    workspace_id: &str,
    plan: &MaintenancePlan,
    explicit_approval: bool,
    operator: &OperatorAuthorization,
    barrier: &mut impl FnMut(SwitchBarrier) -> Result<(), LedgerError>,
) -> Result<Value, MachineError> {
    if !explicit_approval
        || !plan.dry_run
        || plan.kind != "ledger_checkpoint"
        || !plan.retire_content.is_empty()
    {
        return Err(error(
            "INVALID_ARGUMENT",
            "checkpoint apply requires the complete dry-run plan and explicit operator approval",
        ));
    }
    let root = state_root.join("maintenance");
    verify_secure_directory(&root, DarwinSystem.current_uid()).map_err(|_| {
        error(
            "LOOKUP_NAMESPACE_CORRUPT",
            "retained maintenance directory untrusted",
        )
    })?;
    let _workspace = WorkspaceLease::acquire(state_root, true).map_err(ledger_error)?;
    let mut saved = load_operation(&root, workspace_id, plan.plan_ref, operator)?;
    if inspect_namespace(&root, Some(plan.plan_ref))?.0 {
        return Err(error(
            "RUN_BUSY",
            "workspace has another retained maintenance owner",
        ));
    }
    if saved.plan != *plan {
        return Err(error(
            "INVALID_ARGUMENT",
            "apply changed the retained dry-run plan",
        ));
    }
    if saved.state == "completed" {
        return Ok(observation(&saved));
    }
    let owned = retained_owner(&root, &saved)?;
    if !owned && saved.operator_generation != operator.operator_generation {
        return Err(error(
            "OPERATOR_MISMATCH",
            "operator generation changed since planning",
        ));
    }
    if workspace_generation(&root)? != plan.selected_generation {
        if saved.state == "planned" {
            return Err(error(
                "MAINTENANCE_PLAN_STALE",
                "workspace generation changed since planning",
            ));
        }
        // A crash after workspace selection may precede the completed owner record.
        let selected: WorkspaceSelection = read_json(&root.join("selected.json"))?;
        if selected.operation_ref != plan.plan_ref
            || selected.generation != plan.selected_generation + 1
        {
            return Err(error(
                "RECOVERY_REQUIRED",
                "workspace selection no longer belongs to this maintenance",
            ));
        }
    }
    let ids = run_ids(state_root)?;
    if ids != saved.runs.iter().map(|r| r.run_id).collect::<Vec<_>>() {
        return Err(error(
            "MAINTENANCE_PLAN_STALE",
            "workspace Run set changed since planning",
        ));
    }
    let mut lifecycle = Vec::new();
    for basis in &saved.runs {
        lifecycle.push(
            crate::worker::RunLifecycleLease::acquire(state_root, workspace_id, basis.run_id, 1)
                .map_err(|_| error("RUN_BUSY", "Run lifecycle has another owner"))?,
        );
    }
    let writers =
        crate::writer::WriterStore::new(state_root, workspace_id, DarwinSystem.current_uid());
    if writers.load()?.holder.is_some() {
        return Err(error(
            "RUN_BUSY",
            "workspace writer requires its lifecycle owner to settle",
        ));
    }
    let mut anchors: Vec<File> = Vec::new();
    let mut observed = Vec::new();
    for basis in &saved.runs {
        let run_root = state_root.join("runs").join(basis.run_id.to_string());
        verify_secure_file(&run_root.join("audit.jsonl"), DarwinSystem.current_uid())?;
        let anchor = OpenOptions::new()
            .read(true)
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(run_root.join("audit.jsonl"))
            .map_err(|_| error("RECOVERY_REQUIRED", "ledger anchor unavailable"))?;
        DarwinSystem
            .lock_exclusive_nonblocking(&anchor)
            .map_err(|_| error("RUN_BUSY", "ledger has an active writer"))?;
        anchors.push(anchor);
        let (current, ledger, projection, pin) =
            observe_basis(state_root, workspace_id, basis.run_id)?;
        let already_selected = pin.manifest.as_ref().is_some_and(|m| {
            m.generation == basis.ledger_generation + 1
                && m.append_path == format!("ledger-generations/{}/append.jsonl", plan.plan_ref)
        });
        if current.manifest_sha256 != basis.manifest_sha256
            || current.head != basis.head
            || (current.ledger_generation != basis.ledger_generation && !already_selected)
        {
            return Err(error(
                if saved.state == "planned" {
                    "MAINTENANCE_PLAN_STALE"
                } else {
                    "RECOVERY_REQUIRED"
                },
                "Run history or selected generation changed since planning",
            ));
        }
        let (active, unknown) = workload_counts(state_root, basis.run_id)?;
        if active != 0
            || unknown != 0
            || !matches!(
                projection.lifecycle,
                RunLifecycle::Idle
                    | RunLifecycle::Paused
                    | RunLifecycle::Closed
                    | RunLifecycle::StartFailed
            )
            || projection.active_turn_id.is_some()
            || !projection.pending_requests.is_empty()
        {
            return Err(error("RUN_BUSY", "Run has live or unresolved work"));
        }
        let payloads = ledger
            .records()
            .iter()
            .map(|r| r.payload_value().map(|p| (r.kind(), p)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| error("RECOVERY_REQUIRED", "Submit history cannot be verified"))?;
        if !crate::turn::submit_history_is_quiescent(payloads)
            .map_err(|_| error("RECOVERY_REQUIRED", "Submit history cannot be verified"))?
        {
            return Err(error("RUN_BUSY", "Run has unresolved Submit identity"));
        }
        if crate::mutation_admission::active_mutation_admission_records(
            ledger.records(),
            &projection.lifecycle,
        )?
        .is_some()
        {
            return Err(error(
                "RUN_BUSY",
                "Run mutation admission requires recovery",
            ));
        }
        observed.push((run_root, ledger, projection, pin, already_selected));
    }
    retain_owner(&root, &saved)?;
    barrier(SwitchBarrier::OwnerRetained).map_err(ledger_error)?;
    saved.state = "preparing".to_owned();
    persist(&root.join(format!("{}.json", plan.plan_ref)), &saved)?;
    for (run_root, ledger, projection, pin, already_selected) in &observed {
        if *already_selected {
            ledger_generation::retain_activation(run_root, projection.run_id)
                .map_err(ledger_error)?;
            sync_directory(run_root)
                .map_err(|_| error("RECOVERY_REQUIRED", "selected generation sync failed"))?;
        } else {
            ledger_generation::rollover(
                run_root,
                projection.run_id,
                plan.plan_ref,
                pin,
                ledger.records(),
                projection,
                barrier,
            )
            .map_err(ledger_error)?;
        }
        if !saved.completed_runs.contains(&projection.run_id) {
            saved.completed_runs.push(projection.run_id);
        }
        persist(&root.join(format!("{}.json", plan.plan_ref)), &saved)?;
    }
    let mut selected_runs = Vec::new();
    for basis in &saved.runs {
        selected_runs.push(observe_basis(state_root, workspace_id, basis.run_id)?.0);
    }
    saved.selected_manifest_digest = digest(&selected_runs)?;
    persist(
        &root.join("selected.json"),
        &WorkspaceSelection {
            schema: "dolgorae.native-maintenance-selection/v1".to_owned(),
            generation: plan.selected_generation + 1,
            operation_ref: plan.plan_ref,
            runs: selected_runs,
        },
    )?;
    barrier(SwitchBarrier::WorkspaceSelected).map_err(ledger_error)?;
    saved.state = "completed".to_owned();
    persist(&root.join(format!("{}.json", plan.plan_ref)), &saved)?;
    barrier(SwitchBarrier::Completed).map_err(ledger_error)?;
    // Explicit unlock avoids retaining flock in an unrelated inherited descriptor.
    for anchor in &anchors {
        DarwinSystem
            .unlock(anchor)
            .map_err(|_| error("RECOVERY_REQUIRED", "ledger lock release failed"))?;
    }
    drop(lifecycle);
    Ok(observation(&saved))
}
pub fn execute_native(
    command: &str,
    arguments: &[std::ffi::OsString],
) -> Result<Value, MachineError> {
    let carrier =
        crate::controller::carrier_from_options(arguments, "--operator-file", "--operator-fd")?;
    let operator =
        crate::controller::OperatorStore::new(crate::controller::default_operator_root()?)
            .authorize(&carrier)?;
    let workspace = crate::cli::option_path(arguments, "--workspace")
        .map_err(|e| MachineError::invalid_argument("--workspace", e))?;
    let view = crate::workspace::WorkspaceService::system()?.discover(workspace.as_deref())?;
    let home = crate::paths::DolgoraeHome::system()?;
    let state_root = home.workspace_root(&view.workspace_id);
    let generation = || -> Result<u64, MachineError> {
        crate::cli::option_path(arguments, "--selected-generation")
            .map_err(|e| MachineError::invalid_argument("--selected-generation", e))?
            .and_then(|p| p.to_str().and_then(|p| p.parse::<u64>().ok()))
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                MachineError::invalid_argument(
                    "--selected-generation",
                    "positive original workspace generation required",
                )
            })
    };
    let kind = crate::cli::option_path(arguments, "--kind")
        .map_err(|e| MachineError::invalid_argument("--kind", e))?
        .and_then(|p| p.to_str().map(str::to_owned));
    let profile_route = match command {
        "plan_maintenance" => kind.as_deref() == Some("membership_compaction"),
        "apply_maintenance" => {
            let path = crate::cli::option_path(arguments, "--plan-file")
                .map_err(|e| MachineError::invalid_argument("--plan-file", e))?
                .ok_or_else(|| {
                    MachineError::invalid_argument(
                        "--plan-file",
                        "complete retained dry-run plan required",
                    )
                })?;
            read_json_at::<MaintenancePlan>(&path, false)?.kind == "membership_compaction"
        }
        "get_maintenance" => {
            let id = crate::cli::option_path(arguments, "--plan-id")
                .map_err(|e| MachineError::invalid_argument("--plan-id", e))?
                .and_then(|p| p.to_str().and_then(|s| Uuid::parse_str(s).ok()))
                .ok_or_else(|| {
                    MachineError::invalid_argument("--plan-id", "original plan UUIDv7 required")
                })?;
            let root = state_root.join("profile-maintenance");
            [
                format!("{id}.profile.json"),
                format!("{id}.profile.owner.json"),
            ]
            .iter()
            .any(|name| fs::symlink_metadata(root.join(name)).is_ok())
                || trusted_profile_operation(&root, home.root(), id, &view.workspace_id, &operator)
                    .is_some()
        }
        _ => false,
    };
    if profile_route {
        return execute_profile_maintenance(
            command,
            arguments,
            &state_root,
            &view.workspace_id,
            &operator,
        );
    }
    let content_route = match command {
        "plan_maintenance" => kind
            .as_deref()
            .is_some_and(|kind| matches!(kind, "retire_result_content" | "garbage_collection")),
        "apply_maintenance" => {
            let path = crate::cli::option_path(arguments, "--plan-file")
                .map_err(|e| MachineError::invalid_argument("--plan-file", e))?
                .ok_or_else(|| {
                    MachineError::invalid_argument(
                        "--plan-file",
                        "complete retained dry-run plan required",
                    )
                })?;
            matches!(
                read_json_at::<MaintenancePlan>(&path, false)?.kind.as_str(),
                "retire_result_content" | "garbage_collection"
            )
        }
        "get_maintenance" => {
            let id = crate::cli::option_path(arguments, "--plan-id")
                .map_err(|e| MachineError::invalid_argument("--plan-id", e))?
                .and_then(|p| p.to_str().and_then(|s| Uuid::parse_str(s).ok()))
                .ok_or_else(|| {
                    MachineError::invalid_argument("--plan-id", "original plan UUIDv7 required")
                })?;
            let root = state_root.join("result-maintenance");
            fs::symlink_metadata(root.join(format!("{id}.json"))).is_ok()
                || fs::symlink_metadata(root.join(format!("{id}.owner.json"))).is_ok()
        }
        _ => false,
    };
    if content_route {
        let mut data = match command {
            "plan_maintenance" => {
                json!({"plan":plan_result_content(&state_root,&view.workspace_id,generation()?,kind.as_deref().unwrap_or_default(),&operator)?})
            }
            "apply_maintenance" => {
                let path = crate::cli::option_path(arguments, "--plan-file")
                    .map_err(|e| MachineError::invalid_argument("--plan-file", e))?
                    .ok_or_else(|| {
                        MachineError::invalid_argument(
                            "--plan-file",
                            "complete retained plan required",
                        )
                    })?;
                let plan: MaintenancePlan = read_json_at(&path, false)?;
                json!({"maintenance":content_facade_observation(apply_result_content(&state_root,&view.workspace_id,&plan,arguments.iter().any(|a|a=="--approve"),&operator)?)?})
            }
            "get_maintenance" => {
                let id = crate::cli::option_path(arguments, "--plan-id")
                    .map_err(|e| MachineError::invalid_argument("--plan-id", e))?
                    .and_then(|p| p.to_str().and_then(|s| Uuid::parse_str(s).ok()))
                    .ok_or_else(|| {
                        MachineError::invalid_argument("--plan-id", "original plan UUIDv7 required")
                    })?;
                json!({"maintenance":content_facade_observation(get_result_content(&state_root,&view.workspace_id,id,generation()?,&operator)?)?})
            }
            _ => {
                return Err(MachineError::invalid_argument(
                    "command",
                    "unsupported content maintenance command",
                ));
            }
        };
        data["schema"] = json!("dolgorae.external-specialist-response/v5");
        data["operation"] = json!(command);
        return Ok(data);
    }
    if kind
        .as_deref()
        .is_some_and(|kind| kind != "ledger_checkpoint")
    {
        return Err(MachineError::invalid_argument(
            "--kind",
            "unsupported native maintenance kind",
        ));
    }
    let data = match command {
        "plan_maintenance" => {
            json!({"plan":plan_checkpoint(&state_root, &view.workspace_id, generation()?, &operator)?})
        }
        "apply_maintenance" => {
            let plan_path = crate::cli::option_path(arguments, "--plan-file")
                .map_err(|e| MachineError::invalid_argument("--plan-file", e))?
                .ok_or_else(|| {
                    MachineError::invalid_argument(
                        "--plan-file",
                        "complete retained dry-run plan required",
                    )
                })?;
            let plan: MaintenancePlan = read_json_at(&plan_path, false)?;
            json!({"maintenance":apply_checkpoint(&state_root, &view.workspace_id, &plan, arguments.iter().any(|a| a == "--approve"), &operator)?})
        }
        "get_maintenance" => {
            let id = crate::cli::option_path(arguments, "--plan-id")
                .map_err(|e| MachineError::invalid_argument("--plan-id", e))?
                .and_then(|p| p.to_str().and_then(|p| Uuid::parse_str(p).ok()))
                .ok_or_else(|| {
                    MachineError::invalid_argument("--plan-id", "original plan UUIDv7 required")
                })?;
            json!({"maintenance":get_checkpoint(&state_root, &view.workspace_id, id, generation()?, &operator)?})
        }
        _ => {
            return Err(MachineError::invalid_argument(
                "command",
                "unsupported maintenance command",
            ));
        }
    };
    let mut data = data;
    data["schema"] = json!("dolgorae.external-specialist-response/v5");
    data["operation"] = json!(command);
    Ok(data)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileBasis {
    profile: String,
    server_key: String,
    generation: u64,
    journal_sha256: String,
    protected_references: u64,
    writers: u64,
    unknown: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedProfileOperation {
    schema: String,
    plan: MaintenancePlan,
    operator_id: Uuid,
    operator_generation: u64,
    runs: Vec<RunBasis>,
    profiles: Vec<ProfileBasis>,
    bindings_sha256: String,
    state: String,
    completed_profiles: Vec<String>,
    bindings_completed: bool,
    selected_manifest_digest: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileOwner {
    schema: String,
    plan: MaintenancePlan,
    operator_id: Uuid,
    operator_generation: u64,
    source_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileSelection {
    schema: String,
    generation: u64,
    operation_ref: Uuid,
    selected_manifest_digest: String,
}
fn profile_source_digest(saved: &SavedProfileOperation) -> Result<String, MachineError> {
    digest(&(&saved.runs, &saved.profiles, &saved.bindings_sha256))
}
fn profile_owner(saved: &SavedProfileOperation) -> Result<ProfileOwner, MachineError> {
    Ok(ProfileOwner {
        schema: "dolgorae.native-profile-maintenance-owner/v1".into(),
        plan: saved.plan.clone(),
        operator_id: saved.operator_id,
        operator_generation: saved.operator_generation,
        source_sha256: profile_source_digest(saved)?,
    })
}
fn profile_observation(saved: &SavedProfileOperation, pending: bool) -> Value {
    json!({"operation_ref":saved.plan.plan_ref,"plan":saved.plan,
        "state":if saved.state=="preparing" || (saved.state=="planned" && pending) {"recovery_needed"} else {&saved.state},
        "old_generation":saved.plan.selected_generation,"new_generation":if saved.state=="completed" {Some(saved.plan.selected_generation+1)} else {None},
        "selected_manifest_digest":saved.selected_manifest_digest,
        "required_action":if saved.state=="preparing" || pending {"resume_maintenance"} else {"none"}})
}
fn validate_profile_progress(saved: &SavedProfileOperation) -> Result<(), MachineError> {
    let source = profile_source_digest(saved)?;
    let target = digest(&(&saved.profiles, saved.plan.plan_ref))?;
    let finished =
        saved.bindings_completed && saved.completed_profiles.len() == saved.profiles.len();
    if saved.plan.plan_ref.get_version_num() != 7
        || !saved.plan.admission_closed
        || saved.plan.selected_generation.checked_add(1).is_none()
        || saved.completed_profiles.len() > saved.profiles.len()
        || saved
            .completed_profiles
            .iter()
            .zip(&saved.profiles)
            .any(|(key, basis)| key != &basis.server_key)
        || (saved.bindings_completed && saved.completed_profiles.len() != saved.profiles.len())
        || (saved.state == "planned"
            && (!saved.completed_profiles.is_empty() || saved.bindings_completed))
        || (saved.state == "completed" && (!finished || saved.selected_manifest_digest != target))
        || (saved.selected_manifest_digest != source
            && (!finished || saved.selected_manifest_digest != target))
    {
        return Err(error(
            "LOOKUP_NAMESPACE_CORRUPT",
            "Profile maintenance progress contradicts its retained scope",
        ));
    }
    Ok(())
}
fn load_profile_operation(
    root: &Path,
    home_root: &Path,
    workspace: &str,
    id: Uuid,
    operator: &OperatorAuthorization,
) -> Result<SavedProfileOperation, MachineError> {
    let path = root.join(format!("{id}.profile.json"));
    let saved: SavedProfileOperation = read_json(&path)?;
    validate_profile_progress(&saved)?;
    if saved.schema != "dolgorae.native-profile-maintenance-operation/v1"
        || saved.plan.plan_ref != id
        || saved.plan.workspace_ref != workspace
        || saved.operator_id != operator.operator_id
        || saved.operator_generation == 0
        || saved.plan.kind != "membership_compaction"
        || !saved.plan.dry_run
        || !saved.plan.retire_content.is_empty()
        || saved.plan.selected_generation == 0
        || saved.plan.expected_manifest_digest != profile_source_digest(&saved)?
        || !matches!(saved.state.as_str(), "planned" | "preparing" | "completed")
        || saved
            .profiles
            .windows(2)
            .any(|pair| pair[0].server_key >= pair[1].server_key)
        || saved
            .completed_profiles
            .iter()
            .any(|key| !saved.profiles.iter().any(|p| &p.server_key == key))
        || (saved.state == "completed"
            && (!saved.bindings_completed
                || saved.completed_profiles.len() != saved.profiles.len()))
    {
        return Err(error(
            "LOOKUP_NAMESPACE_CORRUPT",
            "retained Profile maintenance scope is invalid",
        ));
    }
    let active_path = home_root.join("profile-maintenance-active.json");
    let active_owner = if fs::symlink_metadata(&active_path).is_ok() {
        Some(read_json::<ProfileOwner>(&active_path)?)
    } else {
        None
    };
    if saved.state != "planned"
        || fs::symlink_metadata(root.join(format!("{id}.profile.owner.json"))).is_ok()
        || active_owner
            .as_ref()
            .is_some_and(|owner| owner.plan.plan_ref == id)
    {
        let owner: ProfileOwner = read_json(&root.join(format!("{id}.profile.owner.json")))?;
        if owner != profile_owner(&saved)? {
            return Err(error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "Profile maintenance owner is invalid",
            ));
        }
    }
    Ok(saved)
}
fn profile_generation(root: &Path) -> Result<u64, MachineError> {
    let mut completed = std::collections::BTreeMap::new();
    for entry in fs::read_dir(root).map_err(|_| {
        error(
            "LOOKUP_NAMESPACE_UNAVAILABLE",
            "Profile maintenance namespace unavailable",
        )
    })? {
        let entry = entry.map_err(|_| {
            error(
                "LOOKUP_NAMESPACE_UNAVAILABLE",
                "Profile maintenance entry unavailable",
            )
        })?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".profile.json"))
        {
            let saved: SavedProfileOperation = read_json(&entry.path())?;
            validate_profile_progress(&saved)?;
            if saved.state == "completed" {
                let generation =
                    saved
                        .plan
                        .selected_generation
                        .checked_add(1)
                        .ok_or_else(|| {
                            error("LOOKUP_NAMESPACE_CORRUPT", "Profile generation overflow")
                        })?;
                if completed.insert(generation, saved.plan.plan_ref).is_some() {
                    return Err(error(
                        "LOOKUP_NAMESPACE_CORRUPT",
                        "duplicate Profile generation owner",
                    ));
                }
            }
        }
    }
    let floor = completed.keys().next_back().copied().unwrap_or(1);
    let path = root.join("profile-selection.json");
    if !path.exists() {
        if floor > 1 {
            return Err(error(
                "LOOKUP_NAMESPACE_UNAVAILABLE",
                "completed Profile maintenance selection is missing",
            ));
        }
        return Ok(1);
    }
    let selection: ProfileSelection = read_json(&path)?;
    let saved: SavedProfileOperation =
        read_json(&root.join(format!("{}.profile.json", selection.operation_ref)))?;
    validate_profile_progress(&saved)?;
    let owner: ProfileOwner =
        read_json(&root.join(format!("{}.profile.owner.json", selection.operation_ref)))?;
    if selection.schema != "dolgorae.native-profile-maintenance-selection/v1"
        || selection.generation < floor
        || selection.generation != saved.plan.selected_generation + 1
        || owner != profile_owner(&saved)?
        || !saved.bindings_completed
        || saved.completed_profiles.len() != saved.profiles.len()
        || selection.selected_manifest_digest != saved.selected_manifest_digest
    {
        return Err(error(
            "LOOKUP_NAMESPACE_CORRUPT",
            "Profile maintenance selection disagrees with its original owner",
        ));
    }
    Ok(selection.generation)
}
fn verify_profile_completed_members(
    home: &crate::paths::DolgoraeHome,
    saved: &SavedProfileOperation,
) -> Result<(), MachineError> {
    let active_path = home.root().join("profile-maintenance-active.json");
    let pending = if fs::symlink_metadata(&active_path).is_ok() {
        let active: ProfileOwner = read_json(&active_path)?;
        active == profile_owner(saved)?
    } else {
        false
    };
    let verify_selected = saved.state != "completed" || pending;
    for basis in saved.profiles.iter().take(saved.completed_profiles.len()) {
        let store = crate::global_runtime::GlobalMembershipStore::new(
            home,
            &basis.profile,
            &basis.server_key,
        )?;
        store.with_maintenance_server_lock(|root, store| {
            store.verify_maintenance_completion_under_server_lock(
                root,
                saved.plan.plan_ref,
                basis.generation,
                &basis.journal_sha256,
                verify_selected,
            )
        })?;
    }
    if saved.bindings_completed {
        crate::global_profile::verify_binding_maintenance_completion(
            home.root(),
            saved.plan.plan_ref,
            &saved.bindings_sha256,
            verify_selected,
        )?;
    }
    Ok(())
}

type ProfileSource = (Vec<RunBasis>, Vec<ProfileBasis>, String, u64, u64, u64);

fn profile_basis(
    home: &crate::paths::DolgoraeHome,
    profile: &str,
    server_key: &str,
) -> Result<ProfileBasis, MachineError> {
    let store = crate::global_runtime::GlobalMembershipStore::new(home, profile, server_key)?;
    store.with_maintenance_server_lock(|root, store| {
        let (generation, index) = store.maintenance_basis_under_server_lock(root)?;
        Ok(ProfileBasis {
            profile: profile.to_owned(),
            server_key: server_key.to_owned(),
            generation,
            journal_sha256: index.journal_sha256,
            protected_references: index
                .members
                .values()
                .filter(|m| m.disposition != crate::global_runtime::MembershipDisposition::Released)
                .count() as u64,
            writers: index.members.values().filter(|m| m.writer).count() as u64,
            unknown: index
                .members
                .values()
                .filter(|m| m.disposition == crate::global_runtime::MembershipDisposition::Unknown)
                .count() as u64,
        })
    })
}

fn profile_bases(
    state_root: &Path,
    workspace: &str,
    home: &crate::paths::DolgoraeHome,
) -> Result<ProfileSource, MachineError> {
    let mut runs = Vec::new();
    let mut selected = std::collections::BTreeMap::new();
    let mut active = 0;
    let mut unknown = 0;
    let store = RunStore::new(SystemWorkspacePlatform, state_root);
    for id in run_ids_with_bootstrap(state_root, true)? {
        let manifest = store.load_manifest(id)?;
        if manifest.workspace_id != workspace {
            return Err(error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "Run belongs to another workspace",
            ));
        }
        let Some(binding) = manifest.global_profile_binding.as_ref() else {
            continue;
        };
        binding.validate_for_recovery()?;
        selected
            .entry(binding.server_key.clone())
            .or_insert(binding.selected_name.clone());
        // Profile compaction never transforms Run history. Its head is planning
        // provenance, not a quiescence or complete-history admission operand.
        runs.push(RunBasis {
            run_id: id,
            manifest_sha256: digest(&manifest)?,
            ledger_generation: ledger_generation::observed_generation(
                &state_root.join("runs").join(id.to_string()),
                id,
            )
            .map_err(ledger_error)?,
            head: store.load_state_projection(id)?.ledger_head,
        });
    }
    let mut profiles = Vec::new();
    for (server_key, profile) in selected {
        let basis = profile_basis(home, &profile, &server_key)?;
        active += basis.writers;
        unknown += basis.unknown;
        profiles.push(basis);
    }
    let (bindings, _) = crate::global_profile::binding_maintenance_basis(home.root())?;
    Ok((runs, profiles, bindings, active, unknown, 0))
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileMaintenanceBarrier {
    OwnerSynced,
    ProgressStarted,
    Membership(crate::global_runtime::MembershipSwitchBarrier),
    MemberProgressSynced,
    Binding(crate::global_profile::BindingMaintenanceBarrier),
    BindingProgressSynced,
    WorkspaceSelected,
    Completed,
}

pub fn apply_profile_with_barrier(
    state_root: &Path,
    workspace: &str,
    home: &crate::paths::DolgoraeHome,
    plan_file: &Path,
    operator: &OperatorAuthorization,
    barrier: &mut impl FnMut(ProfileMaintenanceBarrier) -> Result<(), MachineError>,
) -> Result<Value, MachineError> {
    let args = vec![
        std::ffi::OsString::from("--plan-file"),
        plan_file.as_os_str().to_owned(),
        std::ffi::OsString::from("--approve"),
    ];
    execute_profile_maintenance_at(
        "apply_maintenance",
        &args,
        state_root,
        workspace,
        operator,
        home,
        barrier,
    )
}

fn execute_profile_maintenance(
    command: &str,
    args: &[std::ffi::OsString],
    state_root: &Path,
    workspace: &str,
    operator: &OperatorAuthorization,
) -> Result<Value, MachineError> {
    let home = crate::paths::DolgoraeHome::system()?;
    execute_profile_maintenance_at(
        command,
        args,
        state_root,
        workspace,
        operator,
        &home,
        &mut |_| Ok(()),
    )
}

fn trusted_profile_operation(
    root: &Path,
    home_root: &Path,
    id: Uuid,
    workspace: &str,
    operator: &OperatorAuthorization,
) -> Option<Uuid> {
    [
        root.join(format!("{id}.profile.owner.json")),
        home_root.join("profile-maintenance-active.json"),
    ]
    .iter()
    .find_map(|path| {
        verify_secure_directory(path.parent()?, DarwinSystem.current_uid()).ok()?;
        read_json::<ProfileOwner>(path)
            .ok()
            .filter(|owner| {
                owner.schema == "dolgorae.native-profile-maintenance-owner/v1"
                    && owner.operator_id == operator.operator_id
                    && owner.operator_generation > 0
                    && owner.plan.plan_ref == id
                    && id.get_version_num() == 7
                    && owner.plan.workspace_ref == workspace
                    && owner.plan.kind == "membership_compaction"
                    && owner.plan.dry_run
                    && owner.plan.admission_closed
                    && owner.plan.retire_content.is_empty()
                    && owner.plan.selected_generation > 0
                    && owner.plan.selected_generation.checked_add(1).is_some()
                    && owner
                        .source_sha256
                        .strip_prefix("sha256:")
                        .is_some_and(|s| {
                            s.len() == 64
                                && s.bytes()
                                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        })
                    && owner.source_sha256 == owner.plan.expected_manifest_digest
            })
            .map(|_| id)
    })
}

fn existing_profile_control_root(state_root: &Path, root: &Path) -> Result<(), MachineError> {
    verify_secure_directory(state_root, DarwinSystem.current_uid())?;
    fs::symlink_metadata(root).map_err(|_| {
        error(
            "LOOKUP_NAMESPACE_UNAVAILABLE",
            "retained Profile maintenance namespace unavailable",
        )
    })?;
    verify_secure_directory(root, DarwinSystem.current_uid())
}

fn execute_profile_maintenance_at(
    command: &str,
    args: &[std::ffi::OsString],
    state_root: &Path,
    workspace: &str,
    operator: &OperatorAuthorization,
    home: &crate::paths::DolgoraeHome,
    barrier: &mut impl FnMut(ProfileMaintenanceBarrier) -> Result<(), MachineError>,
) -> Result<Value, MachineError> {
    if home.workspace_root(workspace) != state_root {
        return Err(error(
            "LOOKUP_NAMESPACE_CORRUPT",
            "Profile maintenance home does not own this workspace",
        ));
    }
    let root = state_root.join("profile-maintenance");
    let generation = || {
        crate::cli::option_path(args, "--selected-generation")
            .ok()
            .flatten()
            .and_then(|p| p.to_str().and_then(|s| s.parse::<u64>().ok()))
            .filter(|g| *g > 0)
            .ok_or_else(|| {
                MachineError::invalid_argument(
                    "--selected-generation",
                    "positive original generation required",
                )
            })
    };
    let registry = crate::global_profile::GlobalProfileStore::new(home);
    let original = if command == "apply_maintenance" {
        crate::cli::option_path(args, "--plan-file")
            .ok()
            .flatten()
            .and_then(|path| read_json_at::<MaintenancePlan>(&path, false).ok())
            .map(|plan| plan.plan_ref)
    } else if command == "get_maintenance" {
        crate::cli::option_path(args, "--plan-id")
            .ok()
            .flatten()
            .and_then(|path| path.to_str().and_then(|text| Uuid::parse_str(text).ok()))
    } else {
        None
    };
    let trusted = || {
        original
            .and_then(|id| trusted_profile_operation(&root, home.root(), id, workspace, operator))
    };
    let data = registry.with_maintenance_lock(|_| {
        if command == "plan_maintenance" {
            control_root_named(state_root, "profile-maintenance")?;
        } else {
            existing_profile_control_root(state_root, &root)?;
        }
        match command {
        "plan_maintenance" => {
            if home.root().join("profile-maintenance-active.json").exists() {
                return Err(error("RUN_BUSY", "resume the retained global Profile maintenance before planning another operation"));
            }
            let selected = generation()?;
            if profile_generation(&root)? != selected {
                return Err(error(
                    "MAINTENANCE_PLAN_STALE",
                    "Profile workspace generation changed",
                ));
            }
            let (runs, profiles, bindings, a, u, receipts) =
                profile_bases(state_root, workspace, home)?;
            let plan = MaintenancePlan {
                plan_ref: Uuid::now_v7(),
                workspace_ref: workspace.into(),
                selected_generation: selected,
                dry_run: true,
                kind: "membership_compaction".into(),
                admission_closed: true,
                active_writers: a,
                unknown_workload: u,
                protected_reference_count: profiles.iter().map(|p| p.protected_references).sum(),
                retire_content: Vec::new(),
                retained_receipt_count: receipts,
                expected_manifest_digest: digest(&(&runs, &profiles, &bindings))?,
            };
            let saved = SavedProfileOperation {
                schema: "dolgorae.native-profile-maintenance-operation/v1".into(),
                plan: plan.clone(),
                operator_id: operator.operator_id,
                operator_generation: operator.operator_generation,
                runs,
                profiles,
                bindings_sha256: bindings,
                state: "planned".into(),
                completed_profiles: Vec::new(),
                bindings_completed: false,
                selected_manifest_digest: plan.expected_manifest_digest.clone(),
            };
            persist(
                &root.join(format!("{}.profile.json", plan.plan_ref)),
                &saved,
            )?;
            Ok(json!({"plan":plan}))
        }
        "get_maintenance" => {
            let id = crate::cli::option_path(args, "--plan-id")
                .ok()
                .flatten()
                .and_then(|p| p.to_str().and_then(|s| Uuid::parse_str(s).ok()))
                .ok_or_else(|| {
                    MachineError::invalid_argument("--plan-id", "original UUIDv7 required")
                })?;
            let saved = load_profile_operation(&root, home.root(), workspace, id, operator)?;
            verify_profile_completed_members(home, &saved)?;
            if saved.plan.selected_generation != generation()? {
                return Err(error(
                    "MAINTENANCE_PLAN_STALE",
                    "original Profile generation required",
                ));
            }
            let active_path = home.root().join("profile-maintenance-active.json");
            let pending = if fs::symlink_metadata(&active_path).is_ok() {
                let active: ProfileOwner = read_json(&active_path)?;
                active == profile_owner(&saved)?
            } else { false };
            if saved.state == "preparing" && !pending {
                return Err(error("LOOKUP_NAMESPACE_UNAVAILABLE", "retained Profile admission ownership is missing"));
            }
            let owned = fs::symlink_metadata(root.join(format!("{id}.profile.owner.json"))).is_ok();
            Ok(json!({"maintenance":profile_observation(&saved, pending || (saved.state == "planned" && owned))}))
        }
        "apply_maintenance" => {
            let path = crate::cli::option_path(args, "--plan-file")
                .ok()
                .flatten()
                .ok_or_else(|| {
                    MachineError::invalid_argument("--plan-file", "complete plan required")
                })?;
            let plan: MaintenancePlan = read_json_at(&path, false)?;
            let mut saved = load_profile_operation(&root, home.root(), workspace, plan.plan_ref, operator)?;
            verify_profile_completed_members(home, &saved)?;
            if saved.plan != plan || !args.iter().any(|a| a == "--approve") {
                return Err(MachineError::invalid_argument(
                    "--approve",
                    "exact retained Profile plan and explicit approval required",
                ));
            }
            if saved.state == "completed" {
                let active_path = home.root().join("profile-maintenance-active.json");
                if active_path.exists() {
                    let active: ProfileOwner = read_json(&active_path)?;
                    if active == profile_owner(&saved)? {
                        fs::remove_file(&active_path).and_then(|()|sync_directory(home.root()))
                            .map_err(|_|error("RECOVERY_REQUIRED","completed Profile admission release requires recovery"))?;
                    }
                }
                return Ok(json!({"maintenance":profile_observation(&saved, false)}));
            }
            let active_path = home.root().join("profile-maintenance-active.json");
            let expected_active = profile_owner(&saved)?;
            if active_path.exists() {
                let active: ProfileOwner = read_json(&active_path)?;
                if active != expected_active { return Err(error("RUN_BUSY", "another retained global Profile maintenance owns admission")); }
            } else if saved.state == "preparing" {
                return Err(error("LOOKUP_NAMESPACE_UNAVAILABLE", "retained Profile admission ownership is missing"));
            }
            if saved.state == "planned" {
                let owner_path = root.join(format!("{}.profile.owner.json", plan.plan_ref));
                let owned = fs::symlink_metadata(&owner_path).is_ok();
                if (!owned && saved.operator_generation != operator.operator_generation)
                    || profile_generation(&root)? != plan.selected_generation
                {
                    return Err(error(
                        "MAINTENANCE_PLAN_STALE",
                        "Profile plan authority or generation changed",
                    ));
                }
                let owner_path = root.join(format!("{}.profile.owner.json", plan.plan_ref));
                if owner_path.exists() {
                    let owner: ProfileOwner = read_json(&owner_path)?;
                    if owner != profile_owner(&saved)? {
                        return Err(error("LOOKUP_NAMESPACE_CORRUPT", "Profile owner changed"));
                    }
                } else {
                    let bytes = serde_json::to_vec(&profile_owner(&saved)?)
                        .map_err(|e| error("RECOVERY_REQUIRED", e.to_string()))?;
                    crate::workspace::atomic_create(
                        &SystemWorkspacePlatform,
                        &owner_path,
                        &bytes,
                        0o600,
                    )
                    .map_err(|_| {
                        error("RECOVERY_REQUIRED", "Profile owner retention is uncertain")
                    })?;
                    sync_directory(&root).map_err(|_| {
                        error("RECOVERY_REQUIRED", "Profile owner sync is uncertain")
                    })?;
                }
                if !active_path.exists() {
                    let bytes = serde_json::to_vec(&expected_active).map_err(|e| error("RECOVERY_REQUIRED",e.to_string()))?;
                    crate::workspace::atomic_create(&SystemWorkspacePlatform, &active_path, &bytes, 0o600)
                        .map_err(|_| error("RECOVERY_REQUIRED", "global Profile admission ownership is uncertain"))?;
                    sync_directory(home.root()).map_err(|_| error("RECOVERY_REQUIRED", "global Profile admission sync is uncertain"))?;
                }
                let validated = (|| -> Result<(), MachineError> {
                    for expected in &saved.profiles {
                        let current = profile_basis(home, &expected.profile, &expected.server_key)?;
                        if current.generation != expected.generation
                            || current.journal_sha256 != expected.journal_sha256
                            || current.protected_references != expected.protected_references {
                            return Err(error("MAINTENANCE_PLAN_STALE", "Profile maintenance source changed"));
                        }
                    }
                    let (bindings, _) = crate::global_profile::binding_maintenance_basis(home.root())?;
                    if bindings != saved.bindings_sha256 {
                        return Err(error("MAINTENANCE_PLAN_STALE", "Profile binding source changed"));
                    }
                    Ok(())
                })();
                if let Err(failure) = validated {
                    // No selection or content change precedes this validation.
                    // Keep the immutable owner, but reopen unrelated admission.
                    fs::remove_file(&active_path).and_then(|()| sync_directory(home.root()))
                        .map_err(|_| error("RECOVERY_REQUIRED", "unapplied Profile admission release is uncertain"))?;
                    return Err(failure);
                }
                barrier(ProfileMaintenanceBarrier::OwnerSynced)?;
                saved.state = "preparing".into();
                persist(
                    &root.join(format!("{}.profile.json", plan.plan_ref)),
                    &saved,
                )?;
            }
            barrier(ProfileMaintenanceBarrier::ProgressStarted)?;
            for basis in &saved.profiles {
                if saved.completed_profiles.contains(&basis.server_key) {
                    continue;
                }
                let store = crate::global_runtime::GlobalMembershipStore::new(
                    home,
                    &basis.profile,
                    &basis.server_key,
                )?;
                store.with_maintenance_server_lock(|profile_root, store| {
                    store.compact_under_server_lock(
                        profile_root,
                        plan.plan_ref,
                        basis.generation,
                        &basis.journal_sha256,
                        &mut |point| barrier(ProfileMaintenanceBarrier::Membership(point)),
                    )
                })?;
                saved.completed_profiles.push(basis.server_key.clone());
                persist(
                    &root.join(format!("{}.profile.json", plan.plan_ref)),
                    &saved,
                )?;
            }
            barrier(ProfileMaintenanceBarrier::MemberProgressSynced)?;
            if !saved.bindings_completed {
                crate::global_profile::compact_binding_history(
                    home.root(),
                    plan.plan_ref,
                    &saved.bindings_sha256,
                    &mut |point| barrier(ProfileMaintenanceBarrier::Binding(point)),
                )?;
                saved.bindings_completed = true;
                persist(
                    &root.join(format!("{}.profile.json", plan.plan_ref)),
                    &saved,
                )?;
            }
            barrier(ProfileMaintenanceBarrier::BindingProgressSynced)?;
            saved.selected_manifest_digest = digest(&(&saved.profiles, plan.plan_ref))?;
            persist(
                &root.join(format!("{}.profile.json", plan.plan_ref)),
                &saved,
            )?;
            persist(
                &root.join("profile-selection.json"),
                &ProfileSelection {
                    schema: "dolgorae.native-profile-maintenance-selection/v1".into(),
                    generation: plan.selected_generation + 1,
                    operation_ref: plan.plan_ref,
                    selected_manifest_digest: saved.selected_manifest_digest.clone(),
                },
            )?;
            barrier(ProfileMaintenanceBarrier::WorkspaceSelected)?;
            saved.state = "completed".into();
            persist(
                &root.join(format!("{}.profile.json", plan.plan_ref)),
                &saved,
            )?;
            barrier(ProfileMaintenanceBarrier::Completed)?;
            if active_path.exists() {
                fs::remove_file(&active_path).and_then(|()| sync_directory(home.root()))
                    .map_err(|_| error("RECOVERY_REQUIRED", "completed Profile admission release requires recovery"))?;
            }
            Ok(json!({"maintenance":profile_observation(&saved, false)}))
        }
        _ => Err(MachineError::invalid_argument(
            "command",
            "unsupported Profile maintenance command",
        )),
        }
    }).map_err(|mut failure| {
        if matches!(failure.code.as_str(),"PROFILE_MEMBERSHIP_INCOMPLETE"|"RUNTIME_PATH_INVALID"|"LEGACY_STATE_UNSUPPORTED"|"INTERNAL_ERROR") {
            failure.code="LOOKUP_NAMESPACE_CORRUPT".to_owned();
        }
        if matches!(failure.code.as_str(),"LOOKUP_NAMESPACE_CORRUPT"|"LOOKUP_NAMESPACE_UNAVAILABLE") {
            failure=namespace_failure(failure,trusted());
        } else if failure.code=="RECOVERY_REQUIRED" && trusted().is_some() {
            failure.details=json!({"operation_ref":trusted(),"effect":"unresolved","required_action":"resume_maintenance"});
        }
        failure
    })?;
    let mut data = data;
    data["schema"] = json!("dolgorae.external-specialist-response/v5");
    data["operation"] = json!(command);
    Ok(data)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentDatabaseBasis {
    device: u64,
    inode: u64,
    schema_version: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentOperation {
    schema: String,
    plan: MaintenancePlan,
    operator_id: Uuid,
    operator_generation: u64,
    runs: Vec<RunBasis>,
    database: ContentDatabaseBasis,
    references: Vec<crate::storage_observation::ContentReference>,
    targets: Vec<crate::storage_observation::ContentReference>,
    chunk_bindings: Vec<crate::storage_observation::ChunkBinding>,
    state: String,
    completed_digests: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentOwner {
    schema: String,
    operation_ref: Uuid,
    plan_sha256: String,
    operator_id: Uuid,
    operator_generation: u64,
    workspace_ref: String,
    source_generation: u64,
    database_sha256: String,
    runs_sha256: String,
    references_sha256: String,
    targets_sha256: String,
    chunk_bindings_sha256: String,
}
fn content_owner(saved: &ContentOperation) -> Result<ContentOwner, MachineError> {
    Ok(ContentOwner {
        schema: "dolgorae.native-content-maintenance-owner/v1".into(),
        operation_ref: saved.plan.plan_ref,
        plan_sha256: digest(&saved.plan)?,
        operator_id: saved.operator_id,
        operator_generation: saved.operator_generation,
        workspace_ref: saved.plan.workspace_ref.clone(),
        source_generation: saved.plan.selected_generation,
        database_sha256: digest(&saved.database)?,
        runs_sha256: digest(&saved.runs)?,
        references_sha256: digest(&saved.references)?,
        targets_sha256: digest(&saved.targets)?,
        chunk_bindings_sha256: digest(&saved.chunk_bindings)?,
    })
}
fn content_plan_digest(saved: &ContentOperation) -> Result<String, MachineError> {
    digest(&(
        &saved.runs,
        &saved.database,
        &saved.references,
        &saved.targets,
        &saved.chunk_bindings,
        &saved.plan.retire_content,
    ))
}
fn content_root(state_root: &Path) -> Result<PathBuf, MachineError> {
    let root = state_root.join("result-maintenance");
    match crate::workspace::create_directory(&root, 0o700) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(error(
                "LOOKUP_NAMESPACE_UNAVAILABLE",
                "content maintenance namespace unavailable",
            ));
        }
    }
    verify_secure_directory(&root, DarwinSystem.current_uid())?;
    Ok(root)
}
fn content_plan_bytes_ref(value: &Value) -> Result<(&str, u64), MachineError> {
    let invalid = || {
        error(
            "LOOKUP_NAMESPACE_CORRUPT",
            "content plan byte reference malformed",
        )
    };
    let object = value.as_object().ok_or_else(invalid)?;
    let byte_length = object
        .get("byte_length")
        .and_then(Value::as_u64)
        .filter(|length| *length <= 9_007_199_254_740_991)
        .ok_or_else(invalid)?;
    let sha = object
        .get("sha256")
        .and_then(Value::as_str)
        .and_then(|sha| sha.strip_prefix("sha256:"))
        .filter(|sha| {
            sha.len() == 64
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(invalid)?;
    if object.len() != 2 {
        return Err(invalid());
    }
    Ok((sha, byte_length))
}
fn read_content_operation(
    root: &Path,
    id: Uuid,
    workspace: &str,
    operator: &OperatorAuthorization,
) -> Result<ContentOperation, MachineError> {
    let saved: ContentOperation = read_json(&root.join(format!("{id}.json")))?;
    for value in &saved.plan.retire_content {
        content_plan_bytes_ref(value)?;
    }
    if saved.schema != "dolgorae.native-content-maintenance-operation/v1"
        || saved.plan.plan_ref != id
        || saved.plan.workspace_ref != workspace
        || saved.operator_id != operator.operator_id
        || saved.operator_generation == 0
        || !saved.plan.dry_run
        || !matches!(
            saved.plan.kind.as_str(),
            "retire_result_content" | "garbage_collection"
        )
        || saved.plan.expected_manifest_digest != content_plan_digest(&saved)?
        || !matches!(
            saved.state.as_str(),
            "planned" | "retiring" | "collecting" | "completed"
        )
        || saved.chunk_bindings.len() > 4096
        || saved.chunk_bindings.iter().any(|b| {
            !saved.references.iter().any(|r| {
                r.owner_kind == "brokered_chunk_receipt"
                    && r.owner_id == b.reference_id.to_string()
                    && r.representation == "raw"
                    && r.byte_length == b.byte_length
            })
        })
        || saved.references.len() > 4096
        || saved.targets.len() > saved.references.len()
        || saved
            .targets
            .iter()
            .any(|r| r.retired_operation.is_some() || !saved.references.contains(r))
        || (saved.plan.kind == "garbage_collection" && !saved.targets.is_empty())
        || saved.completed_digests.len() > saved.plan.retire_content.len()
        || saved
            .completed_digests
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != saved.completed_digests.len()
        || saved.completed_digests.iter().any(|d| {
            !saved
                .plan
                .retire_content
                .iter()
                .any(|v| content_plan_bytes_ref(v).is_ok_and(|(sha, _)| sha == d))
        })
    {
        return Err(error(
            "LOOKUP_NAMESPACE_CORRUPT",
            "content maintenance original identity is invalid",
        ));
    }
    if saved.state != "planned" {
        let owner: ContentOwner = read_json(&root.join(format!("{id}.owner.json")))?;
        if owner != content_owner(&saved)? {
            return Err(error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "content maintenance owner disagrees",
            ));
        }
    }
    Ok(saved)
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct AdmissionDirectory {
    device: u64,
    inode: u64,
    uid: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn admission_directory(path: &Path) -> Result<Option<AdmissionDirectory>, MachineError> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(MachineError::runtime_path_invalid(
                path,
                "maintenance directory unavailable",
            ));
        }
        Ok(metadata) => metadata,
    };
    verify_secure_directory(path, DarwinSystem.current_uid())?;
    Ok(Some(AdmissionDirectory {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    }))
}
type AdmissionStamp = [Option<AdmissionDirectory>; 3];
fn admission_stamp(state_root: &Path) -> Result<AdmissionStamp, MachineError> {
    Ok([
        admission_directory(state_root)?,
        admission_directory(&state_root.join("maintenance"))?,
        admission_directory(&state_root.join("result-maintenance"))?,
    ])
}
type AdmissionCache = std::sync::Mutex<Vec<(PathBuf, AdmissionStamp)>>;
static ADMISSION_CACHE: std::sync::OnceLock<AdmissionCache> = std::sync::OnceLock::new();
#[cfg(test)]
thread_local! { static ADMISSION_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub(crate) fn workspace_maintenance_pending(state_root: &Path) -> Result<bool, MachineError> {
    // The caller holds the shared maintenance flock. All supported owner and
    // progress publications replace directory entries under its exclusive side.
    // Never retain a pending/failed result or hold the cache mutex across I/O.
    let before = admission_stamp(state_root)?;
    let cache = ADMISSION_CACHE.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if cache.lock().is_ok_and(|entries| {
        entries
            .iter()
            .any(|(root, stamp)| root == state_root && *stamp == before)
    }) {
        return Ok(false);
    }
    let pending = inspect_workspace_maintenance_pending(state_root)?;
    if !pending
        && before == admission_stamp(state_root)?
        && let Ok(mut entries) = cache.lock()
    {
        entries.retain(|(root, _)| root != state_root);
        if entries.len() >= 64 {
            entries.remove(0);
        }
        entries.push((state_root.to_path_buf(), before));
    }
    Ok(pending)
}
fn inspect_workspace_maintenance_pending(state_root: &Path) -> Result<bool, MachineError> {
    #[cfg(test)]
    ADMISSION_SCANS.with(|count| count.set(count.get() + 1));
    let content_root = state_root.join("result-maintenance");
    let content = content_pending(state_root, None)
        .map_err(|error| admission_namespace_error(&content_root, error))?;
    let checkpoint_root = state_root.join("maintenance");
    let checkpoint = match fs::symlink_metadata(&checkpoint_root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(MachineError::runtime_path_invalid(
                &checkpoint_root,
                "workspace maintenance namespace is unavailable",
            ));
        }
        Ok(_) => {
            verify_secure_directory(&checkpoint_root, DarwinSystem.current_uid())?;
            inspect_checkpoint_namespace(&checkpoint_root, None)
                .map_err(|error| admission_namespace_error(&checkpoint_root, error))?
                .0
        }
    };
    Ok(content || checkpoint)
}
fn admission_namespace_error(root: &Path, error: MachineError) -> MachineError {
    if matches!(
        error.code.as_str(),
        "RUNTIME_PATH_INVALID" | "RUNTIME_PATH_COLLISION" | "STORAGE_CAPACITY_EXCEEDED"
    ) {
        error
    } else {
        MachineError::runtime_path_invalid(
            root,
            "workspace maintenance namespace is unavailable or inconsistent",
        )
    }
}
fn content_pending(state_root: &Path, except: Option<Uuid>) -> Result<bool, MachineError> {
    let root = state_root.join("result-maintenance");
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => {
            return Err(error(
                "LOOKUP_NAMESPACE_UNAVAILABLE",
                "content maintenance namespace unavailable",
            ));
        }
        Ok(_) => {}
    }
    verify_secure_directory(&root, DarwinSystem.current_uid())?;
    let mut pending = false;
    let mut count = 0;
    for entry in fs::read_dir(&root).map_err(|_| {
        error(
            "LOOKUP_NAMESPACE_UNAVAILABLE",
            "content maintenance namespace unreadable",
        )
    })? {
        let entry = entry.map_err(|_| {
            error(
                "LOOKUP_NAMESPACE_UNAVAILABLE",
                "content maintenance namespace unreadable",
            )
        })?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "content maintenance name invalid",
            )
        })?;
        if name.ends_with(".tmp") {
            continue;
        }
        if let Some(id) = name
            .strip_suffix(".owner.json")
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            let owner: ContentOwner = read_json(&entry.path())?;
            let saved: ContentOperation = read_json(&root.join(format!("{id}.json")))?;
            if owner != content_owner(&saved)? {
                return Err(error(
                    "LOOKUP_NAMESPACE_CORRUPT",
                    "content owner namespace incomplete",
                ));
            }
            pending |= saved.state != "completed" && Some(id) != except;
            continue;
        }
        let id = name
            .strip_suffix(".json")
            .and_then(|s| Uuid::parse_str(s).ok())
            .filter(|id| id.get_version_num() == 7)
            .ok_or_else(|| {
                error(
                    "LOOKUP_NAMESPACE_CORRUPT",
                    "content maintenance identity invalid",
                )
            })?;
        let saved: ContentOperation = read_json(&entry.path())?;
        if saved.plan.plan_ref != id
            || saved.plan.expected_manifest_digest != content_plan_digest(&saved)?
        {
            return Err(error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "content maintenance scope invalid",
            ));
        }
        count += 1;
        if count > 100_000 {
            return Err(error(
                "STORAGE_CAPACITY_EXCEEDED",
                "content maintenance namespace bound reached",
            ));
        }
        if saved.state != "planned" {
            let owner: ContentOwner = read_json(&root.join(format!("{id}.owner.json")))?;
            if owner != content_owner(&saved)? {
                return Err(error(
                    "LOOKUP_NAMESPACE_CORRUPT",
                    "content owner namespace incomplete",
                ));
            }
            pending |= saved.state != "completed" && Some(id) != except;
        }
    }
    Ok(pending)
}
fn content_database(
    connection: &rusqlite::Connection,
    path: &Path,
) -> Result<ContentDatabaseBasis, MachineError> {
    use std::os::unix::fs::MetadataExt as _;
    verify_secure_file(path, DarwinSystem.current_uid())?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| error("RECOVERY_REQUIRED", "content database identity unavailable"))?;
    let schema_version = connection
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .map_err(|_| error("RECOVERY_REQUIRED", "content database schema unavailable"))?;
    Ok(ContentDatabaseBasis {
        device: metadata.dev(),
        inode: metadata.ino(),
        schema_version,
    })
}
fn verify_chunk_provenance(connection: &rusqlite::Connection) -> Result<(), MachineError> {
    let exists:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='brokered_tool_results')",[],|r|r.get(0)).map_err(|_|error("RECOVERY_REQUIRED","chunk source namespace unavailable"))?;
    if !exists {
        return Ok(());
    }
    let malformed:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM brokered_tool_results WHERE NOT json_valid(response_json) OR json_extract(response_json,'$.schema_version') IS NOT 'dolgorae.brokered-tool-result/v1' OR (json_extract(response_json,'$.outcome') IS NOT 'ok' AND json_extract(response_json,'$.outcome') IS NOT 'error'))",[],|r|r.get(0)).map_err(|_|error("RECOVERY_REQUIRED","chunk source namespace corrupt"))?;
    if malformed {
        return Err(error(
            "RECOVERY_REQUIRED",
            "chunk source receipts cannot establish absence",
        ));
    }
    let unbound:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM brokered_chunk_bindings c LEFT JOIN brokered_tasks t ON t.task_id=c.task_id LEFT JOIN brokered_tool_results b ON b.session_id=c.session_id AND b.source_tool_call_id=c.source_tool_call_id WHERE t.task_id IS NULL OR t.session_id!=c.session_id OR b.response_json IS NULL OR json_extract(b.response_json,'$.value.task_id') IS NOT c.task_id OR json_extract(b.response_json,'$.value.operation') IS NOT 'read_specialist_result_result' OR json_extract(b.response_json,'$.value.offset') IS NOT c.offset OR json_extract(b.response_json,'$.value.content') IS NOT '')",[],|r|r.get(0)).map_err(|_|error("RECOVERY_REQUIRED","chunk source bindings unavailable"))?;
    if unbound {
        return Err(error(
            "RECOVERY_REQUIRED",
            "chunk source receipt binding is invalid",
        ));
    }
    Ok(())
}
fn all_content_references(
    connection: &rusqlite::Connection,
) -> Result<Vec<crate::storage_observation::ContentReference>, MachineError> {
    let mut statement = connection
        .prepare(
            "SELECT owner_kind,owner_id,representation,sha256,byte_length,retired_operation
         FROM result_content_refs ORDER BY owner_kind,owner_id,representation LIMIT 4097",
        )
        .map_err(|_| {
            error(
                "RECOVERY_REQUIRED",
                "content reference namespace unavailable",
            )
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, u64>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|_| error("RECOVERY_REQUIRED", "content reference query unavailable"))?;
    let mut references = Vec::new();
    for row in rows {
        let (owner_kind, owner_id, representation, sha256, byte_length, retired) =
            row.map_err(|_| error("RECOVERY_REQUIRED", "content reference malformed"))?;
        if Uuid::parse_str(&owner_id)
            .ok()
            .is_none_or(|id| id.get_version_num() != 7)
            || !matches!(
                owner_kind.as_str(),
                "brokered_publication"
                    | "external_artifact"
                    | "legacy_engagement"
                    | "brokered_chunk_receipt"
                    | "research_merge"
                    | "synthesis_task"
            )
            || !match owner_kind.as_str() {
                "research_merge" => {
                    matches!(
                        representation.as_str(),
                        "publication" | "merged_output" | "invocation"
                    )
                }
                "synthesis_task" => representation == "input",
                _ => matches!(representation.as_str(), "json" | "raw"),
            }
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(error(
                "RECOVERY_REQUIRED",
                "content reference identity invalid",
            ));
        }
        references.push(crate::storage_observation::ContentReference {
            owner_kind,
            owner_id,
            representation,
            sha256,
            byte_length,
            retired_operation: retired
                .map(|s| {
                    Uuid::parse_str(&s)
                        .ok()
                        .filter(|id| id.get_version_num() == 7)
                        .ok_or_else(|| error("RECOVERY_REQUIRED", "content tombstone malformed"))
                })
                .transpose()?,
        });
    }
    if references.len() > 4096 {
        return Err(error(
            "STORAGE_CAPACITY_EXCEEDED",
            "content reference bound reached",
        ));
    }
    Ok(references)
}
pub(crate) fn content_delivered(
    connection: &rusqlite::Connection,
    r: &crate::storage_observation::ContentReference,
) -> Result<bool, MachineError> {
    if r.owner_kind == "external_artifact"
        && crate::result_component_artifact::protects_external_artifact(connection, &r.owner_id)?
    {
        return Ok(false);
    }
    if matches!(r.owner_kind.as_str(), "research_merge" | "synthesis_task") {
        return Ok(false);
    }
    let sql = match r.owner_kind.as_str() {
        "brokered_publication" => {
            "SELECT EXISTS(SELECT 1 FROM brokered_tasks t JOIN brokered_result_publications p ON p.task_id=t.task_id JOIN brokered_delivery_receipts d ON d.task_id=t.task_id WHERE t.task_id=?1 AND t.state='delivered' AND p.state='published')
          AND NOT EXISTS(SELECT 1 FROM brokered_tool_results b WHERE json_valid(b.response_json) AND json_extract(b.response_json,'$.outcome')='ok'
          AND json_extract(b.response_json,'$.value.operation')='read_specialist_result_result' AND json_extract(b.response_json,'$.value.task_id')=?1
          AND NOT EXISTS(SELECT 1 FROM brokered_chunk_bindings c WHERE c.session_id=b.session_id AND c.source_tool_call_id=b.source_tool_call_id))"
        }
        "brokered_chunk_receipt" => {
            "SELECT EXISTS(SELECT 1 FROM brokered_chunk_bindings c JOIN brokered_tasks t ON t.task_id=c.task_id JOIN brokered_delivery_receipts d ON d.task_id=t.task_id
             WHERE c.reference_id=?1 AND t.state='delivered'
             AND NOT EXISTS(SELECT 1 FROM brokered_tool_results b WHERE json_valid(b.response_json) AND json_extract(b.response_json,'$.outcome')='ok'
             AND json_extract(b.response_json,'$.value.operation')='read_specialist_result_result' AND json_extract(b.response_json,'$.value.task_id')=t.task_id
             AND NOT EXISTS(SELECT 1 FROM brokered_chunk_bindings normalized WHERE normalized.session_id=b.session_id AND normalized.source_tool_call_id=b.source_tool_call_id)))"
        }
        "external_artifact" => {
            "SELECT EXISTS(SELECT 1 FROM artifacts a JOIN delivery_receipts d ON d.artifact_id=a.artifact_id JOIN tasks t ON t.task_id=d.task_id WHERE a.artifact_id=?1 AND t.state='delivered')"
        }
        "legacy_engagement" => {
            "SELECT EXISTS(SELECT 1 FROM artifacts a JOIN delivery_receipts d ON d.artifact_id=a.artifact_id JOIN tasks t ON t.task_id=d.task_id WHERE a.engagement_id=?1 AND t.state='delivered')"
        }
        _ => return Err(error("RECOVERY_REQUIRED", "content owner kind invalid")),
    };
    connection
        .query_row(sql, [&r.owner_id], |row| row.get(0))
        .map_err(|_| error("RECOVERY_REQUIRED", "content delivery proof unavailable"))
}
fn content_facade_observation(native: Value) -> Result<Value, MachineError> {
    let plan: MaintenancePlan = serde_json::from_value(native["plan"].clone())
        .map_err(|_| error("RECOVERY_REQUIRED", "content observation plan malformed"))?;
    let state = native["state"]
        .as_str()
        .ok_or_else(|| error("RECOVERY_REQUIRED", "content observation state malformed"))?;
    let active = matches!(state, "retiring" | "collecting");
    if !matches!(state, "planned" | "retiring" | "collecting" | "completed") {
        return Err(error(
            "RECOVERY_REQUIRED",
            "content observation state invalid",
        ));
    }
    Ok(
        json!({"operation_ref":plan.plan_ref,"state":if active{"recovery_needed"}else{state},
        "old_generation":plan.selected_generation,"new_generation":null,"selected_manifest_digest":plan.expected_manifest_digest,
        "required_action":if active{"resume_maintenance"}else{"none"},"plan":plan}),
    )
}
fn persist_content_operation(path: &Path, saved: &ContentOperation) -> Result<(), MachineError> {
    let bytes = serde_json::to_vec(saved)
        .map_err(|_| error("RECOVERY_REQUIRED", "content operation encoding failed"))?;
    if bytes.len() as u64 > MAX_CONTROL_BYTES {
        return Err(error(
            "STORAGE_CAPACITY_EXCEEDED",
            "content operation metadata bound reached",
        ));
    }
    persist(path, saved)
}
fn content_observation(saved: &ContentOperation) -> Value {
    json!({"plan":saved.plan,"state":saved.state,"operation_ref":saved.plan.plan_ref,
        "admission_closed":saved.state!="planned"&&saved.state!="completed","effect":if saved.state=="completed"{"content_maintenance_completed"}else{"retained_original_operation"}})
}

pub fn plan_result_content(
    state_root: &Path,
    workspace_id: &str,
    selected_generation: u64,
    kind: &str,
    operator: &OperatorAuthorization,
) -> Result<MaintenancePlan, MachineError> {
    if !matches!(kind, "retire_result_content" | "garbage_collection") {
        return Err(MachineError::invalid_argument(
            "--kind",
            "unsupported content maintenance kind",
        ));
    }
    let checkpoint_root = control_root(state_root)?;
    if inspect_namespace(&checkpoint_root, None)?.0 || content_pending(state_root, None)? {
        return Err(error(
            "RUN_BUSY",
            "resume retained workspace maintenance first",
        ));
    }
    if workspace_generation(&checkpoint_root)? != selected_generation {
        return Err(error(
            "MAINTENANCE_PLAN_STALE",
            "workspace generation changed",
        ));
    }
    let mut runs = Vec::new();
    let mut active_writers = 0;
    let mut unknown_workload = 0;
    let mut retained_receipts = 0;
    for run in run_ids(state_root)? {
        let (basis, ledger, projection, _) = observe_basis(state_root, workspace_id, run)?;
        let (active, unknown) = workload_counts(state_root, run)?;
        active_writers += active;
        unknown_workload +=
            unknown + u64::from(projection.lifecycle == RunLifecycle::OutcomeUnknown);
        retained_receipts += ledger
            .records()
            .iter()
            .filter(|record| {
                matches!(
                    record.kind(),
                    AuditKind::TurnStarted | AuditKind::MutationCompleted
                )
            })
            .count() as u64;
        runs.push(basis);
    }
    let path = crate::engagement::EngagementStore::workspace_database_path(state_root);
    let connection = crate::storage_observation::CheckedConnection::open(&path, true)?;
    let database = content_database(&connection, &path)?;
    verify_chunk_provenance(&connection)?;
    let references = all_content_references(&connection)?;
    let chunk_bindings = crate::storage_observation::chunk_bindings(&connection)?;
    let mut candidates = std::collections::BTreeMap::new();
    let mut targets = Vec::new();
    let mut protected = 0;
    if kind == "retire_result_content" {
        for r in &references {
            if r.retired_operation.is_some() {
                continue;
            }
            if content_delivered(&connection, r)? {
                candidates.insert(r.sha256.clone(), r.byte_length);
                targets.push(r.clone());
            } else {
                protected += 1;
            }
        }
    } else {
        let mut statement=connection.prepare("SELECT c.sha256,c.bytes FROM result_content c WHERE NOT EXISTS(SELECT 1 FROM result_content_refs r WHERE r.sha256=c.sha256 AND r.retired_operation IS NULL) ORDER BY c.sha256 LIMIT 4097")
            .map_err(|_|error("RECOVERY_REQUIRED","content GC query unavailable"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|_| error("RECOVERY_REQUIRED", "content GC query unavailable"))?;
        for row in rows {
            let (sha, bytes) =
                row.map_err(|_| error("RECOVERY_REQUIRED", "content GC row invalid"))?;
            if sha256_hex(&bytes) != sha {
                return Err(error(
                    "ARTIFACT_INTEGRITY_FAILURE",
                    "GC content digest is corrupt",
                ));
            }
            candidates.insert(sha, bytes.len() as u64);
        }
        protected = references
            .iter()
            .filter(|r| r.retired_operation.is_none())
            .count() as u64;
    }
    if candidates.len() > 4096 {
        return Err(error(
            "STORAGE_CAPACITY_EXCEEDED",
            "content plan bound reached",
        ));
    }
    let plan = MaintenancePlan {
        plan_ref: Uuid::now_v7(),
        workspace_ref: workspace_id.into(),
        selected_generation,
        dry_run: true,
        kind: kind.into(),
        admission_closed: false,
        active_writers,
        unknown_workload,
        protected_reference_count: protected,
        retire_content: candidates
            .into_iter()
            .map(|(sha256, byte_length)| json!({"byte_length":byte_length,"sha256":format!("sha256:{sha256}")}))
            .collect(),
        retained_receipt_count: retained_receipts,
        expected_manifest_digest: String::new(),
    };
    let mut saved = ContentOperation {
        schema: "dolgorae.native-content-maintenance-operation/v1".into(),
        plan,
        operator_id: operator.operator_id,
        operator_generation: operator.operator_generation,
        runs,
        database,
        references,
        targets,
        chunk_bindings,
        state: "planned".into(),
        completed_digests: Vec::new(),
    };
    saved.plan.expected_manifest_digest = content_plan_digest(&saved)?;
    let root = content_root(state_root)?;
    persist_content_operation(&root.join(format!("{}.json", saved.plan.plan_ref)), &saved)?;
    Ok(saved.plan)
}
pub fn get_result_content(
    state_root: &Path,
    workspace_id: &str,
    plan_ref: Uuid,
    selected_generation: u64,
    operator: &OperatorAuthorization,
) -> Result<Value, MachineError> {
    let root = state_root.join("result-maintenance");
    verify_secure_directory(&root, DarwinSystem.current_uid())?;
    let mut saved = read_content_operation(&root, plan_ref, workspace_id, operator)?;
    if saved.plan.selected_generation != selected_generation {
        return Err(MachineError::invalid_argument(
            "--selected-generation",
            "original generation required",
        ));
    }
    if saved.state == "planned"
        && fs::symlink_metadata(root.join(format!("{plan_ref}.owner.json"))).is_ok()
    {
        let owner: ContentOwner = read_json(&root.join(format!("{plan_ref}.owner.json")))?;
        if owner != content_owner(&saved)? {
            return Err(error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "content owner binding changed",
            ));
        }
        saved.state = if saved.plan.kind == "garbage_collection" {
            "collecting"
        } else {
            "retiring"
        }
        .into();
    }
    Ok(content_observation(&saved))
}
fn apply_result_content_inner(
    state_root: &Path,
    workspace_id: &str,
    plan: &MaintenancePlan,
    explicit_approval: bool,
    operator: &OperatorAuthorization,
    barrier: &mut impl FnMut(ContentMaintenanceBarrier) -> Result<(), MachineError>,
) -> Result<Value, MachineError> {
    if !explicit_approval
        || !plan.dry_run
        || !matches!(
            plan.kind.as_str(),
            "retire_result_content" | "garbage_collection"
        )
    {
        return Err(MachineError::invalid_argument(
            "--approve",
            "exact content dry-run plan and explicit approval required",
        ));
    }
    let root = state_root.join("result-maintenance");
    verify_secure_directory(&root, DarwinSystem.current_uid())?;
    let mut saved = read_content_operation(&root, plan.plan_ref, workspace_id, operator)?;
    if saved.plan != *plan {
        return Err(MachineError::invalid_argument(
            "--plan-file",
            "complete unchanged retained plan required",
        ));
    }
    if saved.state == "completed" {
        return Ok(content_observation(&saved));
    }
    let owner_path = root.join(format!("{}.owner.json", plan.plan_ref));
    let owned = match fs::symlink_metadata(&owner_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(error(
                "LOOKUP_NAMESPACE_UNAVAILABLE",
                "content owner unavailable",
            ));
        }
        Ok(_) => {
            let owner: ContentOwner = read_json(&owner_path)?;
            if owner != content_owner(&saved)? {
                return Err(error("LOOKUP_NAMESPACE_CORRUPT", "content owner mismatch"));
            }
            true
        }
    };
    if !owned && saved.operator_generation != operator.operator_generation {
        return Err(error("OPERATOR_MISMATCH", "operator generation changed"));
    }
    let _workspace = WorkspaceLease::acquire(state_root, true).map_err(ledger_error)?;
    let checkpoint_root = control_root(state_root)?;
    if inspect_namespace(&checkpoint_root, Some(plan.plan_ref))?.0
        || content_pending(state_root, Some(plan.plan_ref))?
    {
        return Err(error(
            "RUN_BUSY",
            "another retained maintenance owner exists",
        ));
    }
    if workspace_generation(&checkpoint_root)? != plan.selected_generation {
        return Err(error(
            "MAINTENANCE_PLAN_STALE",
            "workspace generation changed",
        ));
    }
    if run_ids(state_root)?
        != saved
            .runs
            .iter()
            .map(|basis| basis.run_id)
            .collect::<Vec<_>>()
    {
        return Err(error("MAINTENANCE_PLAN_STALE", "workspace Run set changed"));
    }
    let mut lifecycle = Vec::new();
    let mut anchors = Vec::new();
    let writers =
        crate::writer::WriterStore::new(state_root, workspace_id, DarwinSystem.current_uid());
    if writers.load()?.holder.is_some() {
        return Err(error("RUN_BUSY", "workspace writer remains owned"));
    }
    for basis in &saved.runs {
        lifecycle.push(
            crate::worker::RunLifecycleLease::acquire(state_root, workspace_id, basis.run_id, 1)
                .map_err(|_| error("RUN_BUSY", "Run lifecycle remains owned"))?,
        );
        let audit = state_root
            .join("runs")
            .join(basis.run_id.to_string())
            .join("audit.jsonl");
        verify_secure_file(&audit, DarwinSystem.current_uid())?;
        let anchor = OpenOptions::new()
            .read(true)
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&audit)
            .map_err(|_| error("RECOVERY_REQUIRED", "Run audit anchor unavailable"))?;
        DarwinSystem
            .lock_exclusive_nonblocking(&anchor)
            .map_err(|_| error("RUN_BUSY", "Run audit remains owned"))?;
        anchors.push(anchor);
        let (current, ledger, projection, _) =
            observe_basis(state_root, workspace_id, basis.run_id)?;
        if current != *basis {
            return Err(error(
                "MAINTENANCE_PLAN_STALE",
                "Run manifest or history changed",
            ));
        }
        let (active, unknown) = workload_counts(state_root, basis.run_id)?;
        if active != 0
            || unknown != 0
            || !matches!(
                projection.lifecycle,
                RunLifecycle::Idle
                    | RunLifecycle::Paused
                    | RunLifecycle::Closed
                    | RunLifecycle::StartFailed
            )
            || projection.active_turn_id.is_some()
            || !projection.pending_requests.is_empty()
        {
            return Err(error("RUN_BUSY", "Run has live or unresolved work"));
        }
        let payloads = ledger
            .records()
            .iter()
            .map(|r| r.payload_value().map(|p| (r.kind(), p)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| error("RECOVERY_REQUIRED", "Run history cannot be verified"))?;
        if !crate::turn::submit_history_is_quiescent(payloads)
            .map_err(|_| error("RECOVERY_REQUIRED", "Run intent cannot be verified"))?
            || crate::mutation_admission::active_mutation_admission_records(
                ledger.records(),
                &projection.lifecycle,
            )
            .map_err(|_| error("RECOVERY_REQUIRED", "Run admission cannot be verified"))?
            .is_some()
        {
            return Err(error("RUN_BUSY", "Run has accepted unresolved work"));
        }
    }
    let path = crate::engagement::EngagementStore::workspace_database_path(state_root);
    let mut connection = crate::storage_observation::CheckedConnection::open_maintenance(&path)?;
    if content_database(&connection, &path)? != saved.database {
        return Err(error(
            "MAINTENANCE_PLAN_STALE",
            "content database replaced or schema changed",
        ));
    }
    verify_chunk_provenance(&connection)?;
    if crate::storage_observation::chunk_bindings(&connection)? != saved.chunk_bindings {
        return Err(error(
            "MAINTENANCE_PLAN_STALE",
            "chunk provenance bindings changed",
        ));
    }
    let mut current = all_content_references(&connection)?;
    for reference in &mut current {
        if reference.retired_operation == Some(plan.plan_ref) {
            reference.retired_operation = None;
        }
    }
    if current != saved.references {
        return Err(error(
            "MAINTENANCE_PLAN_STALE",
            "content references changed",
        ));
    }
    if !owned {
        crate::workspace::atomic_create(
            &SystemWorkspacePlatform,
            &owner_path,
            &serde_json::to_vec(&content_owner(&saved)?)
                .map_err(|_| error("RECOVERY_REQUIRED", "content owner encoding failed"))?,
            0o600,
        )
        .map_err(|_| error("RECOVERY_REQUIRED", "content owner retention failed"))?;
        barrier(ContentMaintenanceBarrier::OwnerRetained)?;
    }
    saved.state = if plan.kind == "garbage_collection" {
        "collecting"
    } else {
        "retiring"
    }
    .into();
    persist_content_operation(&root.join(format!("{}.json", plan.plan_ref)), &saved)?;
    if plan.kind == "retire_result_content" {
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| error("RECOVERY_REQUIRED", "retirement DB ownership unavailable"))?;
        for reference in &saved.targets {
            if !content_delivered(&transaction, reference)? {
                return Err(error(
                    "RUN_BUSY",
                    "approved retirement target is no longer delivered",
                ));
            }
            if !plan.retire_content.iter().any(|v| {
                content_plan_bytes_ref(v).is_ok_and(|(sha, length)| {
                    sha == reference.sha256 && length == reference.byte_length
                })
            }) {
                return Err(error(
                    "MAINTENANCE_PLAN_STALE",
                    "eligible retirement target changed",
                ));
            }
            transaction.execute("UPDATE result_content_refs SET retired_operation=?4 WHERE owner_kind=?1 AND owner_id=?2 AND representation=?3 AND (retired_operation IS NULL OR retired_operation=?4)",
                rusqlite::params![reference.owner_kind,reference.owner_id,reference.representation,plan.plan_ref.to_string()]).map_err(|_|error("RECOVERY_REQUIRED","content retirement failed"))?;
        }
        crate::storage_observation::commit(transaction)
            .map_err(|_| error("RECOVERY_REQUIRED", "retirement commit failed"))?;
        barrier(ContentMaintenanceBarrier::TombstonesCommitted)?;
        saved.completed_digests = plan
            .retire_content
            .iter()
            .map(|v| content_plan_bytes_ref(v).map(|(sha, _)| sha.to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
    } else {
        for value in &plan.retire_content {
            let (sha, byte_length) = content_plan_bytes_ref(value)?;
            if saved.completed_digests.iter().any(|d| d == sha) {
                continue;
            }
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|_| error("RECOVERY_REQUIRED", "GC DB ownership unavailable"))?;
            let live:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM result_content_refs WHERE sha256=?1 AND retired_operation IS NULL)",[sha],|row|row.get(0)).map_err(|_|error("RECOVERY_REQUIRED","GC protected reference query failed"))?;
            if live {
                return Err(error("RUN_BUSY", "GC content remains protected"));
            }
            use rusqlite::OptionalExtension as _;
            let bytes: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT bytes FROM result_content WHERE sha256=?1",
                    [sha],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| error("RECOVERY_REQUIRED", "GC content read unavailable"))?;
            if let Some(bytes) = bytes {
                if sha256_hex(&bytes) != sha || bytes.len() as u64 != byte_length {
                    return Err(error(
                        "ARTIFACT_INTEGRITY_FAILURE",
                        "GC candidate content changed",
                    ));
                }
            } else if !owned {
                return Err(error(
                    "MAINTENANCE_PLAN_STALE",
                    "GC candidate disappeared before approved operation",
                ));
            }
            transaction
                .execute("DELETE FROM result_content WHERE sha256=?1", [sha])
                .map_err(|_| error("RECOVERY_REQUIRED", "GC content deletion failed"))?;
            crate::storage_observation::commit(transaction)
                .map_err(|_| error("RECOVERY_REQUIRED", "GC deletion commit failed"))?;
            barrier(ContentMaintenanceBarrier::ContentRowDeleted)?;
            let body = state_root.join("result-content").join(format!("{sha}.bin"));
            match fs::symlink_metadata(&body) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(error(
                        "RECOVERY_REQUIRED",
                        "GC content identity unavailable",
                    ));
                }
                Ok(_) => {
                    verify_secure_directory(
                        &state_root.join("result-content"),
                        DarwinSystem.current_uid(),
                    )?;
                    verify_secure_file(&body, DarwinSystem.current_uid())?;
                    use std::os::unix::fs::MetadataExt as _;
                    if fs::symlink_metadata(&body)
                        .map_err(|_| error("RECOVERY_REQUIRED", "GC content identity unavailable"))?
                        .nlink()
                        != 1
                    {
                        return Err(error("RECOVERY_REQUIRED", "GC content alias is untrusted"));
                    }
                    let mut file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                        .open(&body)
                        .map_err(|_| error("RECOVERY_REQUIRED", "GC body read unavailable"))?;
                    let metadata = file
                        .metadata()
                        .map_err(|_| error("RECOVERY_REQUIRED", "GC body identity unavailable"))?;
                    if metadata.len() > 32 * 1024 * 1024 {
                        return Err(error(
                            "ARTIFACT_INTEGRITY_FAILURE",
                            "GC body exceeds content bound",
                        ));
                    }
                    let before = (
                        metadata.dev(),
                        metadata.ino(),
                        metadata.len(),
                        metadata.ctime(),
                        metadata.ctime_nsec(),
                    );
                    let mut body_bytes = Vec::new();
                    file.read_to_end(&mut body_bytes)
                        .map_err(|_| error("RECOVERY_REQUIRED", "GC body read unavailable"))?;
                    let after = file
                        .metadata()
                        .map_err(|_| error("RECOVERY_REQUIRED", "GC body identity unavailable"))?;
                    if before
                        != (
                            after.dev(),
                            after.ino(),
                            after.len(),
                            after.ctime(),
                            after.ctime_nsec(),
                        )
                        || sha256_hex(&body_bytes) != sha
                        || body_bytes.len() as u64 != byte_length
                    {
                        return Err(error("ARTIFACT_INTEGRITY_FAILURE", "GC body changed"));
                    }
                    fs::remove_file(&body)
                        .map_err(|_| error("RECOVERY_REQUIRED", "GC content unlink failed"))?;
                    barrier(ContentMaintenanceBarrier::BodyUnlinked)?;
                    sync_directory(&state_root.join("result-content"))
                        .map_err(|_| error("RECOVERY_REQUIRED", "GC content unlink sync failed"))?;
                }
            }
            saved.completed_digests.push(sha.to_owned());
            persist_content_operation(&root.join(format!("{}.json", plan.plan_ref)), &saved)?;
            barrier(ContentMaintenanceBarrier::ProgressSynced)?;
        }
    }
    saved.state = "completed".into();
    persist_content_operation(&root.join(format!("{}.json", plan.plan_ref)), &saved)?;
    barrier(ContentMaintenanceBarrier::Completed)?;
    Ok(content_observation(&saved))
}

pub fn apply_result_content_with_barrier(
    state_root: &Path,
    workspace_id: &str,
    plan: &MaintenancePlan,
    explicit_approval: bool,
    operator: &OperatorAuthorization,
    mut barrier: impl FnMut(ContentMaintenanceBarrier) -> Result<(), MachineError>,
) -> Result<Value, MachineError> {
    apply_result_content_inner(
        state_root,
        workspace_id,
        plan,
        explicit_approval,
        operator,
        &mut barrier,
    )
    .map_err(|mut failure| {
        let root = state_root.join("result-maintenance");
        if let Ok(saved) = read_content_operation(&root, plan.plan_ref, workspace_id, operator)
            && saved.plan == *plan
            && let Ok(owner) =
                read_json::<ContentOwner>(&root.join(format!("{}.owner.json", plan.plan_ref)))
            && content_owner(&saved).is_ok_and(|expected| expected == owner)
            && let Some(details) = failure.details.as_object_mut()
        {
            details.insert("operation_ref".into(), json!(plan.plan_ref));
            details.insert("effect".into(), json!("unresolved"));
            details.insert("required_action".into(), json!("resume_maintenance"));
        }
        failure
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentMaintenanceBarrier {
    OwnerRetained,
    TombstonesCommitted,
    ContentRowDeleted,
    BodyUnlinked,
    ProgressSynced,
    Completed,
}
pub fn apply_result_content(
    state_root: &Path,
    workspace_id: &str,
    plan: &MaintenancePlan,
    explicit_approval: bool,
    operator: &OperatorAuthorization,
) -> Result<Value, MachineError> {
    apply_result_content_with_barrier(
        state_root,
        workspace_id,
        plan,
        explicit_approval,
        operator,
        |_| Ok(()),
    )
}

#[cfg(test)]
mod content_tests {
    use super::*;
    use crate::controller::{CredentialCarrier, OperatorStore};
    const WORKSPACE_ID: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    #[test]
    fn task085_merge_invocation_preserves_4096_4097_inventory_bound() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE result_content_refs(owner_kind TEXT,owner_id TEXT,representation TEXT,sha256 TEXT,byte_length INTEGER,retired_operation TEXT)").unwrap();
        let owner = Uuid::now_v7().to_string();
        for representation in ["publication", "merged_output", "invocation"] {
            connection
                .execute(
                    "INSERT INTO result_content_refs VALUES('research_merge',?1,?2,?3,1,NULL)",
                    rusqlite::params![owner, representation, "a".repeat(64)],
                )
                .unwrap();
        }
        for _ in 3..4096 {
            connection.execute("INSERT INTO result_content_refs VALUES('research_merge',?1,'invocation',?2,1,NULL)",rusqlite::params![Uuid::now_v7().to_string(),"a".repeat(64)]).unwrap();
        }
        let references = all_content_references(&connection).unwrap();
        assert_eq!(references.len(), 4096);
        for reference in references
            .iter()
            .filter(|reference| reference.owner_id == owner)
        {
            assert!(!content_delivered(&connection, reference).unwrap());
        }
        connection.execute("INSERT INTO result_content_refs VALUES('research_merge',?1,'invocation',?2,1,NULL)",rusqlite::params![Uuid::now_v7().to_string(),"a".repeat(64)]).unwrap();
        assert_eq!(
            all_content_references(&connection).unwrap_err().code,
            "STORAGE_CAPACITY_EXCEEDED"
        );
    }
    #[test]
    fn shared_admission_reuses_history_and_detects_owner_publications() {
        let fixture = ContentFixture::new();
        let root = control_root(&fixture.root).unwrap();
        let mut last = None;
        for generation in 1..=200 {
            let runs = Vec::<RunBasis>::new();
            let saved = SavedOperation {
                schema: "dolgorae.native-maintenance-operation/v1".into(),
                plan: MaintenancePlan {
                    plan_ref: Uuid::now_v7(),
                    workspace_ref: WORKSPACE_ID.into(),
                    selected_generation: generation,
                    dry_run: true,
                    kind: "ledger_checkpoint".into(),
                    admission_closed: true,
                    active_writers: 0,
                    unknown_workload: 0,
                    protected_reference_count: 0,
                    retire_content: Vec::new(),
                    retained_receipt_count: 0,
                    expected_manifest_digest: digest(&runs).unwrap(),
                },
                operator_id: fixture.operator.operator_id,
                operator_generation: fixture.operator.operator_generation,
                runs,
                state: "completed".into(),
                completed_runs: Vec::new(),
                selected_manifest_digest: String::new(),
            };
            persist(&root.join(format!("{}.json", saved.plan.plan_ref)), &saved).unwrap();
            // Completion normally follows retained ownership. Build that valid
            // historical pair directly without running 200 empty checkpoints.
            persist(
                &root.join(format!("{}.owner.json", saved.plan.plan_ref)),
                &expected_owner(&saved).unwrap(),
            )
            .unwrap();
            last = Some(saved);
        }
        let before = ADMISSION_SCANS.with(|count| count.get());
        for _ in 0..100 {
            drop(WorkspaceLease::acquire(&fixture.root, false).unwrap());
        }
        assert_eq!(ADMISSION_SCANS.with(|count| count.get()) - before, 1);
        let mut pending = last.unwrap();
        pending.plan.plan_ref = Uuid::now_v7();
        pending.plan.selected_generation = 201;
        pending.state = "planned".into();
        let path = root.join(format!("{}.json", pending.plan.plan_ref));
        persist(&path, &pending).unwrap();
        retain_owner(&root, &pending).unwrap();
        assert!(matches!(
            WorkspaceLease::acquire(&fixture.root, false),
            Err(LedgerError::WriterBusy(_))
        ));
        pending.state = "completed".into();
        // The same temp+rename publication used by older binaries invalidates
        // the cached namespace without any cache-specific writer cooperation.
        persist(&path, &pending).unwrap();
        drop(WorkspaceLease::acquire(&fixture.root, false).unwrap());
        fs::remove_file(root.join(format!("{}.owner.json", pending.plan.plan_ref))).unwrap();
        assert!(matches!(
            WorkspaceLease::acquire(&fixture.root, false),
            Err(LedgerError::SecurityPolicy(_))
        ));
    }
    struct ContentFixture {
        root: PathBuf,
        operator: OperatorAuthorization,
        first: Uuid,
        second: Uuid,
        sha: String,
    }
    impl ContentFixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("dolgorae-content-maintenance-{}", Uuid::now_v7()));
            for path in [
                &root,
                &root.join("runtime"),
                &root.join("runtime/locks"),
                &root.join("runs"),
                &root.join("result-content"),
            ] {
                fs::create_dir(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            fs::write(root.join("workspace.json"), b"{}").unwrap();
            fs::set_permissions(
                root.join("workspace.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            crate::writer::WriterStore::initialize_layout(
                &root,
                WORKSPACE_ID,
                DarwinSystem.current_uid(),
            )
            .unwrap();
            let store = OperatorStore::new(root.join("operator"));
            let credential = root.join("operator.json");
            store.initialize(&credential).unwrap();
            let operator = store
                .authorize(&CredentialCarrier::open_path(&credential).unwrap())
                .unwrap();
            let path = crate::engagement::EngagementStore::workspace_database_path(&root);
            drop(crate::engagement::EngagementStore::open(&path).unwrap());
            let mut connection =
                crate::storage_observation::CheckedConnection::open(&path, false).unwrap();
            let transaction = connection.transaction().unwrap();
            let body = b"{\"result\":\"same bytes\"}";
            let sha = sha256_hex(body);
            let first = Uuid::now_v7();
            let second = Uuid::now_v7();
            for artifact in [first, second] {
                let engagement = Uuid::now_v7();
                let run = Uuid::now_v7();
                let task = Uuid::now_v7();
                let hire = Uuid::now_v7();
                transaction.execute("INSERT INTO engagements(engagement_id,workspace_id,controller_sha256,state,revision) VALUES(?1,?2,?3,'result_ready',1)",rusqlite::params![engagement.to_string(),WORKSPACE_ID,"a".repeat(64)]).unwrap();
                transaction.execute("INSERT INTO members(specialist_run_id,engagement_id,hire_operation_id,configuration_sha256,state) VALUES(?1,?2,?3,?4,'ready')",rusqlite::params![run.to_string(),engagement.to_string(),hire.to_string(),"b".repeat(64)]).unwrap();
                transaction.execute("INSERT INTO tasks(task_id,engagement_id,specialist_run_id,objective_sha256,state,result_sha256) VALUES(?1,?2,?3,?4,'delivered',?5)",rusqlite::params![task.to_string(),engagement.to_string(),run.to_string(),"c".repeat(64),sha]).unwrap();
                transaction.execute("INSERT INTO artifacts(artifact_id,engagement_id,task_id,specialist_run_id,result_sha256,canonical_json) VALUES(?1,?2,?3,?4,?5,'')",rusqlite::params![artifact.to_string(),engagement.to_string(),task.to_string(),run.to_string(),sha]).unwrap();
                transaction.execute("INSERT INTO delivery_receipts(task_id,artifact_id,delivered_at_ms) VALUES(?1,?2,1)",rusqlite::params![task.to_string(),artifact.to_string()]).unwrap();
                crate::storage_observation::put_content(
                    &transaction,
                    "external_artifact",
                    &artifact.to_string(),
                    "json",
                    body,
                )
                .unwrap();
            }
            crate::storage_observation::commit(transaction).unwrap();
            drop(connection);
            let shared = root.join("result-content").join(format!("{sha}.bin"));
            fs::write(&shared, body).unwrap();
            fs::set_permissions(&shared, fs::Permissions::from_mode(0o600)).unwrap();
            Self {
                root,
                operator,
                first,
                second,
                sha,
            }
        }
        fn plan(&self, kind: &str) -> MaintenancePlan {
            plan_result_content(&self.root, WORKSPACE_ID, 1, kind, &self.operator).unwrap()
        }
        fn apply(&self, plan: &MaintenancePlan) -> Value {
            apply_result_content(&self.root, WORKSPACE_ID, plan, true, &self.operator).unwrap()
        }
        fn database(&self) -> crate::storage_observation::CheckedConnection {
            crate::storage_observation::CheckedConnection::open(
                &crate::engagement::EngagementStore::workspace_database_path(&self.root),
                true,
            )
            .unwrap()
        }
    }
    #[test]
    fn a_base_only_generation_six_database_plans_content_maintenance() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-base-only-content-{}", Uuid::now_v7()));
        for path in [
            &root,
            &root.join("runtime"),
            &root.join("runtime/locks"),
            &root.join("runs"),
            &root.join("result-content"),
        ] {
            fs::create_dir_all(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(root.join("workspace.json"), b"{}").unwrap();
        fs::set_permissions(
            root.join("workspace.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        crate::writer::WriterStore::initialize_layout(
            &root,
            WORKSPACE_ID,
            DarwinSystem.current_uid(),
        )
        .unwrap();
        let store = OperatorStore::new(root.join("operator"));
        let credential = root.join("operator.json");
        store.initialize(&credential).unwrap();
        let operator = store
            .authorize(&CredentialCarrier::open_path(&credential).unwrap())
            .unwrap();
        // Exactly what a workspace that never hosted a Broker carries: the base
        // generation-6 database created by the engagement opener.
        let path = crate::engagement::EngagementStore::workspace_database_path(&root);
        drop(crate::engagement::EngagementStore::open(&path).unwrap());
        let connection = crate::storage_observation::CheckedConnection::open(&path, true).unwrap();
        assert!(
            crate::storage_observation::chunk_bindings(&connection)
                .unwrap()
                .is_empty()
        );
        drop(connection);
        // Content maintenance therefore plans instead of failing on the absent
        // orchestration extension.
        let plan =
            plan_result_content(&root, WORKSPACE_ID, 1, "garbage_collection", &operator).unwrap();
        assert_eq!(plan.workspace_ref, WORKSPACE_ID);
        let retire =
            plan_result_content(&root, WORKSPACE_ID, 1, "retire_result_content", &operator)
                .unwrap();
        assert_eq!(retire.kind, "retire_result_content");
        std::fs::remove_dir_all(root).unwrap();
    }

    impl Drop for ContentFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    #[test]
    fn retained_content_plan_rejects_malformed_wire_digest_before_effects() {
        for malformed in ["d".repeat(64), format!("sha256:{}", "D".repeat(64))] {
            let fixture = ContentFixture::new();
            let plan = fixture.plan("retire_result_content");
            assert_eq!(
                plan.retire_content[0]["sha256"],
                format!("sha256:{}", fixture.sha)
            );
            let root = fixture.root.join("result-maintenance");
            let path = root.join(format!("{}.json", plan.plan_ref));
            let mut saved: ContentOperation = read_json(&path).unwrap();
            saved.plan.retire_content[0]["sha256"] = json!(malformed);
            saved.plan.expected_manifest_digest = content_plan_digest(&saved).unwrap();
            persist_content_operation(&path, &saved).unwrap();
            let failure = apply_result_content(
                &fixture.root,
                WORKSPACE_ID,
                &saved.plan,
                true,
                &fixture.operator,
            )
            .unwrap_err();
            assert_eq!(failure.code, "LOOKUP_NAMESPACE_CORRUPT");
            assert!(!root.join(format!("{}.owner.json", plan.plan_ref)).exists());
            let connection = fixture.database();
            assert_eq!(connection.query_row("SELECT COUNT(*) FROM result_content_refs WHERE retired_operation IS NOT NULL", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
            assert_eq!(
                crate::storage_observation::read_content(
                    &connection,
                    "external_artifact",
                    &fixture.first.to_string(),
                    "json"
                )
                .unwrap(),
                Some(b"{\"result\":\"same bytes\"}".to_vec())
            );
        }
    }
    #[test]
    fn content_planning_rejects_non_v7_tombstone_identity() {
        let fixture = ContentFixture::new();
        let path = crate::engagement::EngagementStore::workspace_database_path(&fixture.root);
        let connection = crate::storage_observation::CheckedConnection::open(&path, false).unwrap();
        connection
            .execute(
                "UPDATE result_content_refs SET retired_operation=?1 WHERE owner_id=?2",
                rusqlite::params![
                    "123e4567-e89b-42d3-a456-426614174000",
                    fixture.first.to_string()
                ],
            )
            .unwrap();
        drop(connection);
        let failure = plan_result_content(
            &fixture.root,
            WORKSPACE_ID,
            1,
            "garbage_collection",
            &fixture.operator,
        )
        .unwrap_err();
        assert_eq!(failure.code, "RECOVERY_REQUIRED");
        assert!(!fixture.root.join("result-maintenance").exists());
    }
    #[test]
    fn equal_content_keeps_distinct_owners_and_receipts_and_explicit_retirement() {
        let fixture = ContentFixture::new();
        let connection = fixture.database();
        assert_ne!(fixture.first, fixture.second);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM result_content", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM delivery_receipts", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            2
        );
        drop(connection);
        let plan = fixture.plan("retire_result_content");
        assert_eq!(plan.retire_content.len(), 1);
        let saved: ContentOperation = read_json(
            &fixture
                .root
                .join("result-maintenance")
                .join(format!("{}.json", plan.plan_ref)),
        )
        .unwrap();
        assert_eq!(saved.targets.len(), 2);
        assert_eq!(fixture.apply(&plan)["state"], "completed");
        let connection = fixture.database();
        assert_eq!(
            crate::storage_observation::read_content(
                &connection,
                "external_artifact",
                &fixture.first.to_string(),
                "json"
            )
            .unwrap_err()
            .code,
            "RESULT_CONTENT_UNAVAILABLE"
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            2
        );
        drop(connection);
        assert!(
            fixture
                .root
                .join("result-content")
                .join(format!("{}.bin", fixture.sha))
                .exists()
        );
        let gc = fixture.plan("garbage_collection");
        assert_eq!(gc.retire_content.len(), 1);
        fixture.apply(&gc);
        assert!(
            !fixture
                .root
                .join("result-content")
                .join(format!("{}.bin", fixture.sha))
                .exists()
        );
        let connection = fixture.database();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM result_content", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM result_content_refs", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    fn reader_pin_excludes_retirement_and_plan_does_not_retire_undelivered_equal_body() {
        let fixture = ContentFixture::new();
        let connection = crate::storage_observation::CheckedConnection::open(
            &crate::engagement::EngagementStore::workspace_database_path(&fixture.root),
            false,
        )
        .unwrap();
        connection.execute("UPDATE tasks SET state='completed_not_delivered' WHERE task_id=(SELECT task_id FROM artifacts WHERE artifact_id=?1)",[fixture.second.to_string()]).unwrap();
        drop(connection);
        let plan = fixture.plan("retire_result_content");
        assert_eq!(plan.protected_reference_count, 1);
        let reader = fixture.database();
        assert_eq!(
            apply_result_content(&fixture.root, WORKSPACE_ID, &plan, true, &fixture.operator)
                .unwrap_err()
                .code,
            "RUN_BUSY"
        );
        drop(reader);
        fixture.apply(&plan);
        let connection = fixture.database();
        assert!(
            crate::storage_observation::read_content(
                &connection,
                "external_artifact",
                &fixture.second.to_string(),
                "json"
            )
            .unwrap()
            .is_some()
        );
        drop(connection);
        let gc = fixture.plan("garbage_collection");
        assert!(gc.retire_content.is_empty());
    }
    #[test]
    fn retirement_recovers_original_operation_at_each_durable_boundary() {
        for barrier in [
            ContentMaintenanceBarrier::OwnerRetained,
            ContentMaintenanceBarrier::TombstonesCommitted,
            ContentMaintenanceBarrier::Completed,
        ] {
            let fixture = ContentFixture::new();
            let plan = fixture.plan("retire_result_content");
            let mut fired = false;
            let failure = apply_result_content_with_barrier(
                &fixture.root,
                WORKSPACE_ID,
                &plan,
                true,
                &fixture.operator,
                |observed| {
                    if observed == barrier && !fired {
                        fired = true;
                        Err(error("RECOVERY_REQUIRED", "injected content boundary"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(fired);
            assert_eq!(failure.details["operation_ref"], plan.plan_ref.to_string());
            let before = get_result_content(
                &fixture.root,
                WORKSPACE_ID,
                plan.plan_ref,
                1,
                &fixture.operator,
            )
            .unwrap();
            assert_eq!(before["operation_ref"], plan.plan_ref.to_string());
            // The process lock has been released by the injected failure, but
            // ordinary writers and readers must not stale the retained plan.
            let blocked = barrier != ContentMaintenanceBarrier::Completed;
            assert_eq!(
                WorkspaceLease::for_run(
                    &fixture.root.join("runs").join(Uuid::now_v7().to_string())
                )
                .is_err(),
                blocked
            );
            let database =
                crate::engagement::EngagementStore::workspace_database_path(&fixture.root);
            for read_only in [false, true] {
                assert_eq!(
                    crate::storage_observation::CheckedConnection::open(&database, read_only)
                        .is_err(),
                    blocked
                );
            }
            assert_eq!(fixture.apply(&plan)["state"], "completed");
            assert!(WorkspaceLease::acquire(&fixture.root, false).is_ok());
            assert_eq!(
                fixture.apply(&plan)["operation_ref"],
                plan.plan_ref.to_string()
            );
        }
    }
    #[test]
    fn garbage_collection_recovers_after_delete_unlink_and_progress_without_changing_identity() {
        for barrier in [
            ContentMaintenanceBarrier::OwnerRetained,
            ContentMaintenanceBarrier::ContentRowDeleted,
            ContentMaintenanceBarrier::BodyUnlinked,
            ContentMaintenanceBarrier::ProgressSynced,
            ContentMaintenanceBarrier::Completed,
        ] {
            let fixture = ContentFixture::new();
            let retirement = fixture.plan("retire_result_content");
            fixture.apply(&retirement);
            let plan = fixture.plan("garbage_collection");
            let mut fired = false;
            let failure = apply_result_content_with_barrier(
                &fixture.root,
                WORKSPACE_ID,
                &plan,
                true,
                &fixture.operator,
                |observed| {
                    if observed == barrier && !fired {
                        fired = true;
                        Err(error("RECOVERY_REQUIRED", "injected GC boundary"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(fired);
            assert_eq!(failure.details["operation_ref"], plan.plan_ref.to_string());
            let blocked = barrier != ContentMaintenanceBarrier::Completed;
            assert_eq!(
                WorkspaceLease::acquire(&fixture.root, false).is_err(),
                blocked
            );
            let database =
                crate::engagement::EngagementStore::workspace_database_path(&fixture.root);
            assert_eq!(
                crate::storage_observation::CheckedConnection::open(&database, false).is_err(),
                blocked
            );
            assert_eq!(fixture.apply(&plan)["state"], "completed");
            let connection = fixture.database();
            assert_eq!(
                connection
                    .query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get::<_, u64>(0))
                    .unwrap(),
                2
            );
            assert_eq!(
                connection
                    .query_row("SELECT COUNT(*) FROM delivery_receipts", [], |r| r
                        .get::<_, u64>(0))
                    .unwrap(),
                2
            );
        }
    }
    #[test]
    fn changed_exact_owner_set_and_unapproved_apply_prevent_retirement() {
        let fixture = ContentFixture::new();
        let plan = fixture.plan("retire_result_content");
        assert_eq!(
            apply_result_content(&fixture.root, WORKSPACE_ID, &plan, false, &fixture.operator)
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
        let connection = crate::storage_observation::CheckedConnection::open(
            &crate::engagement::EngagementStore::workspace_database_path(&fixture.root),
            false,
        )
        .unwrap();
        connection
            .execute(
                "UPDATE result_content_refs SET owner_id=?1 WHERE owner_id=?2",
                rusqlite::params![Uuid::now_v7().to_string(), fixture.first.to_string()],
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            apply_result_content(&fixture.root, WORKSPACE_ID, &plan, true, &fixture.operator)
                .unwrap_err()
                .code,
            "MAINTENANCE_PLAN_STALE"
        );
    }
}
