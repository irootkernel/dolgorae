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
    verify_secure_directory(&state_root.join("runs"), DarwinSystem.current_uid())?;
    let mut runs = Vec::new();
    for entry in fs::read_dir(state_root.join("runs"))
        .map_err(|_| error("RECOVERY_REQUIRED", "Run namespace unavailable"))?
    {
        let entry = entry.map_err(|_| error("RECOVERY_REQUIRED", "Run namespace unreadable"))?;
        let name = entry.file_name();
        // RunStore owns temporary bootstrap directories; they are not accepted Runs.
        if name.to_str().is_some_and(|name| name.starts_with('.')) {
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

fn profile_bases(
    state_root: &Path,
    workspace: &str,
    home: &crate::paths::DolgoraeHome,
) -> Result<ProfileSource, MachineError> {
    let mut runs = Vec::new();
    let mut selected = std::collections::BTreeMap::new();
    let mut active = 0;
    let mut unknown = 0;
    let mut receipts = 0;
    for id in run_ids(state_root)? {
        let (basis, ledger, projection, _) = observe_basis(state_root, workspace, id)?;
        let (a, u) = workload_counts(state_root, id)?;
        active += a;
        unknown += u + u64::from(projection.lifecycle == RunLifecycle::OutcomeUnknown);
        receipts += ledger
            .records()
            .iter()
            .filter(|record| {
                matches!(
                    record.kind(),
                    AuditKind::TurnStarted | AuditKind::MutationCompleted
                )
            })
            .count() as u64;
        let manifest = RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(id)?;
        let binding = manifest.global_profile_binding.ok_or_else(|| {
            error(
                "LOOKUP_NAMESPACE_CORRUPT",
                "Run lacks immutable Profile binding",
            )
        })?;
        binding.validate_for_recovery()?;
        selected
            .entry(binding.server_key)
            .or_insert(binding.selected_name);
        runs.push(basis);
    }
    let mut profiles = Vec::new();
    for (server_key, profile) in selected {
        let store = crate::global_runtime::GlobalMembershipStore::new(home, &profile, &server_key)?;
        let basis = store.with_maintenance_server_lock(|root, store| {
            let (generation, index) = store.maintenance_basis_under_server_lock(root)?;
            Ok(ProfileBasis {
                profile: profile.clone(),
                server_key: server_key.clone(),
                generation,
                journal_sha256: index.journal_sha256,
                protected_references: index
                    .members
                    .values()
                    .filter(|m| {
                        m.disposition != crate::global_runtime::MembershipDisposition::Released
                    })
                    .count() as u64,
                writers: index.members.values().filter(|m| m.writer).count() as u64,
                unknown: index
                    .members
                    .values()
                    .filter(|m| {
                        m.disposition != crate::global_runtime::MembershipDisposition::Released
                    })
                    .count() as u64,
            })
        })?;
        active += basis.writers;
        unknown += basis.unknown;
        profiles.push(basis);
    }
    let (bindings, _) = crate::global_profile::binding_maintenance_basis(home.root())?;
    Ok((runs, profiles, bindings, active, unknown, receipts))
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
            let _lease = WorkspaceLease::acquire(state_root, true).map_err(ledger_error)?;
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
            let _lease = WorkspaceLease::acquire(state_root, true).map_err(ledger_error)?;
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
                    let (runs, profiles, bindings, a, u, _) =
                    profile_bases(state_root, workspace, home)?;
                if a != 0 || u != 0 {
                    return Err(error(
                        "RUN_BUSY",
                        "Profile maintenance cannot bypass active or unknown work",
                    ));
                }
                if runs != saved.runs
                    || profiles != saved.profiles
                    || bindings != saved.bindings_sha256
                {
                    return Err(error(
                        "MAINTENANCE_PLAN_STALE",
                        "Profile maintenance source changed",
                    ));
                }
                    Ok(())
                })();
                if let Err(failure) = validated {
                    if matches!(failure.code.as_str(), "MAINTENANCE_PLAN_STALE" | "RUN_BUSY") {
                        fs::remove_file(&active_path).and_then(|()| sync_directory(home.root()))
                            .map_err(|_| error("RECOVERY_REQUIRED", "unapplied Profile admission release is uncertain"))?;
                    }
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
