//! Machine-local Runtime Profile registry and Codex App Server lifecycle.

use crate::app_server::{JsonRpcConnection, TransportError};
use crate::darwin::DarwinSystem;
use crate::jcs::{PayloadRepresentation, canonicalize, parse, represent_payload};
use crate::machine::MachineError;
use crate::workspace::{
    LocalProfileRegistry, NativeSubagents, RuntimeProfile, WorkspaceService, parse_local_profiles,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{
    FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MANIFEST: &str = include_str!("../docs/protocol/codex-0.149.0-required-subset.json");
const SUPPORTED_CODEX_VERSION: &str = "0.149.0";
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: u64 = 8 * 1024 * 1024;
/// One diagnostic record is bounded so a single observation can never
/// consume the journal a whole server key shares.
const MAX_DIAGNOSTIC_RECORD_BYTES: usize = 4096;
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// How long a freshly spawned app-server has to bind its Unix socket before
/// the start gives up. Spent entirely outside the home and server locks.
const SOCKET_BIND_BUDGET: Duration = Duration::from_secs(10);
/// How long a signalled process group has to exit after `SIGTERM`, and then
/// after `SIGKILL`. Both waits happen outside the home and server locks:
/// ADR-016 rejects waiting for member quiescence under `server.lock` because
/// member shutdown paths need that same lock and would deadlock against it.
const TERMINATE_BUDGET: Duration = Duration::from_secs(5);
const KILL_BUDGET: Duration = Duration::from_secs(2);

/// The closed set of profile-specific Codex capability names `profile doctor`
/// and `profile show` report. Unproven names stay `Unverified` rather than
/// being omitted, so the snapshot is always closed.
const PROFILE_CAPABILITY_NAMES: [&str; 6] = [
    "account_read",
    "app_server_initialize",
    "early_response_id",
    "model_list",
    "native_subagent_lifecycle",
    "thread_absence_error",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileView {
    pub name: String,
    pub argv: Vec<String>,
    pub codex_home: String,
    pub environment: BTreeMap<String, String>,
    pub native_subagents: String,
}

/// `profile show`'s result: the static profile view plus the closed
/// profile-specific Codex capability snapshot SPEC-013 requires.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileShowResult {
    pub name: String,
    pub argv: Vec<String>,
    pub codex_home: String,
    pub environment: BTreeMap<String, String>,
    pub native_subagents: String,
    pub capabilities: BTreeMap<String, ProfileCapabilityState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableIdentity {
    pub resolved_path: String,
    pub device: u64,
    pub inode: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSnapshot {
    pub schema_version: u32,
    pub profile_name: String,
    pub canonical_codex_home: String,
    pub normalized_argv: Vec<String>,
    pub launch_cwd_policy: String,
    pub derived_launch_cwd: String,
    pub sanitized_environment: BTreeMap<String, String>,
    pub enabled_features: Vec<String>,
    pub disabled_features: Vec<String>,
    pub process_static_configuration: BTreeMap<String, Value>,
    pub initial_configuration_observation: BTreeMap<String, Value>,
    pub executable_identity: ExecutableIdentity,
    pub codex_version: String,
    pub schema_bundle_sha256: String,
    pub compatibility_manifest_sha256: String,
    pub launch_contract_sha256: String,
    pub compatibility_verdict: CompatibilityVerdict,
    pub server_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityVerdict {
    Tested,
    Unverified,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileCapabilityState {
    Supported,
    RecognizedUnsupported,
    Unavailable,
    Unverified,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerState {
    pub schema_version: u32,
    pub server_key: String,
    pub lifecycle: String,
    pub server_epoch: u64,
    pub epoch_id: Uuid,
    pub pid: u32,
    pub pgid: u32,
    pub uid: u32,
    pub process_fingerprint: String,
    pub drainer_pid: u32,
    pub drainer_pgid: u32,
    pub drainer_uid: u32,
    pub drainer_fingerprint: String,
    pub socket_path: String,
    pub socket_device: u64,
    pub socket_inode: u64,
    pub membership_revision: u64,
    pub default_model: String,
    pub models: Vec<String>,
    pub capabilities: BTreeMap<String, ProfileCapabilityState>,
    pub snapshot: ProfileSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeActive {
    schema_version: u32,
    canonical_codex_home: String,
    server_key: String,
    server_epoch: u64,
    lifecycle: String,
    pid: Option<u32>,
    /// The token PREPARE stamped on this account home.
    ///
    /// A start or stop spends its long waits — spawn, socket bind, probe,
    /// `SIGTERM` quiescence — with the home and server locks released, so
    /// COMMIT cannot assume the home is still the one PREPARE reserved. It
    /// reacquires the locks and requires this token, the server key, and the
    /// epoch to be unchanged before it publishes anything. A record written
    /// before the token existed reads back as `None` and can therefore never
    /// satisfy a commit that is looking for one.
    #[serde(default)]
    transition_token: Option<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipRecord {
    schema_version: u32,
    revision: u64,
    kind: String,
    workspace_id: String,
    run_id: Option<String>,
    previous_sha256: String,
    record_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipIndex {
    schema_version: u32,
    revision: u64,
    journal_sha256: String,
    active_members: BTreeMap<String, MembershipRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorResult {
    pub profile: String,
    pub compatibility: CompatibilityVerdict,
    pub codex_version: Option<String>,
    pub expected_codex_home: String,
    pub schema_bundle_sha256: Option<String>,
    pub manifest_sha256: Option<String>,
    pub server_key: Option<String>,
    pub launch_probe: bool,
    pub server_started: bool,
    pub server_epoch: Option<u64>,
    pub default_model: Option<String>,
    pub models: Vec<String>,
    pub capabilities: BTreeMap<String, ProfileCapabilityState>,
    pub diagnostics: Vec<Value>,
}

struct Context {
    workspace_id: String,
    registry_path: PathBuf,
    application_support_root: PathBuf,
}

#[derive(Debug)]
struct ConfigurationSnapshot {
    launch: BTreeMap<String, Value>,
    observation: BTreeMap<String, Value>,
}

struct ProbeResult {
    default_model: String,
    models: Vec<String>,
    capabilities: BTreeMap<String, ProfileCapabilityState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileOperation {
    Add,
    List,
    Show,
    Remove,
    Doctor,
    ServerStatus,
    ServerStart,
    ServerStop,
    ServerRestart,
    ServerMigrate,
    MembershipVerify,
    MembershipTombstoneOrphan,
    StateReset,
    DiagnosticsList,
    Events,
}

pub const PROFILE_LOG_DRAINER_COMMAND: &str = "__profile-log-drainer";

pub fn execute(operation: ProfileOperation, arguments: &[OsString]) -> Result<Value, MachineError> {
    let parsed = Parsed::new(arguments, operation)?;
    match operation {
        ProfileOperation::Add => add(&parsed),
        ProfileOperation::List => list(&parsed),
        ProfileOperation::Show => show(&parsed),
        ProfileOperation::Remove => remove(&parsed),
        ProfileOperation::Doctor => doctor(&parsed),
        ProfileOperation::ServerStatus => server_status(&parsed),
        ProfileOperation::ServerStart => server_start(&parsed),
        ProfileOperation::ServerStop => server_stop(&parsed, false),
        ProfileOperation::ServerRestart => server_restart(&parsed),
        ProfileOperation::ServerMigrate => server_migrate(&parsed),
        ProfileOperation::MembershipVerify => membership_verify(&parsed),
        ProfileOperation::MembershipTombstoneOrphan => membership_tombstone(&parsed),
        ProfileOperation::StateReset => state_reset(&parsed),
        ProfileOperation::DiagnosticsList => diagnostics(&parsed, false),
        ProfileOperation::Events => diagnostics(&parsed, true),
    }
}

/// Validates profile command arguments against the same shape and
/// mutually-exclusive-carrier rules `execute` enforces, without running the
/// operation. Callers that only need to know whether an invocation is
/// well-formed can use this instead of `execute`.
pub fn validate_arguments(
    operation: ProfileOperation,
    arguments: &[OsString],
) -> Result<(), MachineError> {
    Parsed::new(arguments, operation).map(|_| ())
}

pub fn run_log_drainer(root: &Path) -> Result<(), MachineError> {
    verify_private_directory(root)?;
    let log_path = root.join("server.log");
    if log_path.exists() {
        verify_private_regular_file(&log_path, 0o600)?;
    } else {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&log_path)
            .map_err(io_error)?;
        verify_private_regular_file(&log_path, 0o600)?;
    }
    let stdout = File::open("/dev/fd/3").map_err(io_error)?;
    let stderr = File::open("/dev/fd/4").map_err(io_error)?;
    let (sender, receiver) = std::sync::mpsc::channel::<(&'static str, Option<Vec<u8>>)>();
    for (name, mut source) in [("stdout", stdout), ("stderr", stderr)] {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match source.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = sender.send((name, None));
                        break;
                    }
                    Ok(count) => {
                        if sender.send((name, Some(buffer[..count].to_vec()))).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    drop(sender);
    let mut buffers = BTreeMap::from([("stdout", Vec::new()), ("stderr", Vec::new())]);
    let mut ended = BTreeSet::new();
    let mut sink_available = true;
    while ended.len() < 2 {
        let (name, chunk) = receiver
            .recv()
            .map_err(|_| transport("profile log drainer channels closed early"))?;
        let buffer = buffers.get_mut(name).expect("known stream");
        let Some(chunk) = chunk else {
            if !buffer.is_empty() && sink_available {
                sink_available = write_log_line(root, name, buffer).is_ok();
            }
            buffer.clear();
            ended.insert(name);
            continue;
        };
        buffer.extend_from_slice(&chunk);
        while let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            let line = buffer.drain(..=index).collect::<Vec<_>>();
            if sink_available {
                sink_available = write_log_line(root, name, &line).is_ok();
            }
        }
        if buffer.len() > usize::try_from(MAX_LOG_BYTES).expect("1 MiB fits usize") {
            if sink_available {
                sink_available =
                    write_log_line(root, name, b"[DOLGORAE_LOG_LINE_DROPPED]\n").is_ok();
            }
            buffer.clear();
        }
    }
    if !sink_available {
        let _ = append_diagnostic(root, "log_sink_degraded", json!({}));
    }
    Ok(())
}

fn write_log_line(root: &Path, stream: &str, raw: &[u8]) -> Result<(), MachineError> {
    let body = format!("[{stream}] {}\n", redacted_log_body(raw));
    let path = root.join("server.log");
    let current = fs::metadata(&path).map_or(0, |metadata| metadata.len());
    if current.saturating_add(body.len() as u64) > MAX_LOG_BYTES {
        let rotated = root.join("server.log.1");
        if rotated.exists() {
            fs::remove_file(&rotated).map_err(io_error)?;
        }
        if path.exists() {
            fs::rename(&path, &rotated).map_err(io_error)?;
            fs::set_permissions(&rotated, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        }
    }
    let mut sink = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(io_error)?;
    verify_private_regular_file(&path, 0o600)?;
    sink.write_all(body.as_bytes()).map_err(io_error)
}

/// The `$dolgorae_redacted` marker the JSON redactor writes, rendered for a
/// plain-text log line.
const REDACTED_TEXT_MARKER: &str = "[DOLGORAE_REDACTED]";
/// The marker that replaces a line redaction could not be proven complete on.
const DROPPED_LOG_MARKER: &str = "[DOLGORAE_LOG_LINE_DROPPED]";

/// Renders one captured app-server log line for the private server log.
///
/// A Codex app-server writes ordinary diagnostic text, not JSON, so treating
/// "did not parse as JSON" as "could not be redacted" discarded essentially
/// every line the drainer captured and left an operator with a log full of
/// drop markers. Redaction is what the log owes: a JSON line goes through the
/// canonical redacting representation, and any other line is kept with its
/// secret-bearing assignments replaced. The drop marker is reserved for a line
/// redaction genuinely could not be applied to — one that is not UTF-8, or one
/// whose JSON shape defeated the canonical redactor — because a marker that
/// means "unredactable" and a marker that means "not JSON" cannot both be the
/// same marker without making the first one meaningless.
fn redacted_log_body(raw: &[u8]) -> String {
    let trimmed = raw.strip_suffix(b"\n").unwrap_or(raw);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    let Ok(text) = std::str::from_utf8(trimmed) else {
        return DROPPED_LOG_MARKER.to_owned();
    };
    if text.trim_start().starts_with(['{', '[']) {
        return match represent_payload(text.as_bytes()) {
            PayloadRepresentation::Represented {
                canonical_bytes, ..
            } => String::from_utf8_lossy(&canonical_bytes).into_owned(),
            PayloadRepresentation::Unrepresentable(_) => DROPPED_LOG_MARKER.to_owned(),
        };
    }
    redact_text_line(text)
}

/// Keeps a plain-text line up to the first `name:`/`name=` whose name reads as
/// a secret, and replaces everything after that name with one marker.
///
/// It asks the same question the JSON redactor asks — is this *name* a
/// secret-bearing one — rather than guessing from value shapes, so the two
/// paths cannot disagree about what a name means. It then redacts to end of
/// line rather than to the next space, because a secret's value is not
/// reliably one whitespace-delimited token: `Authorization: Bearer <token>`
/// would leak its token to any rule that stopped at the first space.
/// Over-redacting the tail of a line costs operator detail; under-redacting it
/// costs the secret.
fn redact_text_line(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(offset) = rest.find([':', '=']) {
        let (name, remainder) = rest.split_at(offset);
        output.push_str(name);
        output.push_str(&remainder[..1]);
        let value = &remainder[1..];
        if is_secret_assignment_name(name) {
            let leading = value.len() - value.trim_start().len();
            output.push_str(&value[..leading]);
            if !value.trim().is_empty() {
                output.push_str(REDACTED_TEXT_MARKER);
            }
            return output;
        }
        rest = value;
    }
    output.push_str(rest);
    output
}

/// The tail token of `name` treated as an assignment name, matched against the
/// same secret-name vocabulary the JSON redactor uses.
fn is_secret_assignment_name(name: &str) -> bool {
    let token = name
        .rsplit(|character: char| {
            character.is_whitespace() || matches!(character, ',' | ';' | '"' | '\'' | '[' | '(')
        })
        .find(|candidate| !candidate.is_empty())
        .unwrap_or_default();
    !token.is_empty() && crate::jcs::is_secret_payload_key(token)
}

fn add(parsed: &Parsed) -> Result<Value, MachineError> {
    let name = parsed.required_positional(0, "profile name")?;
    let context = context(parsed.path("--workspace")?.as_deref())?;
    let codex_home = parsed.required("--codex-home")?;
    if !Path::new(&codex_home).is_absolute() {
        return Err(MachineError::profile_config_invalid(
            &context.registry_path,
            "--codex-home must be absolute",
        ));
    }
    if parsed.required("--native-subagents")? != "enabled" {
        return Err(MachineError::new(
            "NATIVE_SUBAGENT_DISABLE_UNAVAILABLE",
            "public Runtime Profiles require native subagents enabled",
            false,
            json!({
                "profile": name,
                "capability_name": "native_subagent_disable",
                "support": "unavailable",
            }),
        ));
    }
    let argv = parsed.trailing.clone();
    if argv.is_empty() {
        return Err(MachineError::invalid_argument(
            "argv",
            "a direct Codex executable is required after --",
        ));
    }
    let environment = parsed.environment()?;
    let _config_lock = config_lock(&context)?;
    let mut registry = load_registry(&context)?;
    if registry.profiles.contains_key(&name) {
        return Err(MachineError::new(
            "PROFILE_ALREADY_EXISTS",
            "profile already exists",
            false,
            json!({"profile": name}),
        ));
    }
    registry.profiles.insert(
        name.clone(),
        RuntimeProfile {
            argv,
            codex_home,
            environment,
            native_subagents: NativeSubagents::Enabled,
        },
    );
    store_registry(&context, &registry)?;
    let validated = parse_local_profiles(&context.registry_path)?;
    let profile = validated.profiles.get(&name).expect("stored profile");
    serde_json::to_value(profile_view(&name, profile)).map_err(internal)
}

fn list(parsed: &Parsed) -> Result<Value, MachineError> {
    parsed.reject_positionals_and_trailing()?;
    let context = context(parsed.path("--workspace")?.as_deref())?;
    let registry = load_registry(&context)?;
    let profiles = registry
        .profiles
        .iter()
        .map(|(name, profile)| profile_view(name, profile))
        .collect::<Vec<_>>();
    Ok(json!({"profiles": profiles}))
}

fn show(parsed: &Parsed) -> Result<Value, MachineError> {
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let _ = context;
    let view = profile_view(&name, &profile);
    // `profile show` reports validation-derived fields without executing the
    // profile (SPEC-013), so its capability snapshot is always the explicit
    // unverified baseline rather than a probed or stored one.
    serde_json::to_value(ProfileShowResult {
        name: view.name,
        argv: view.argv,
        codex_home: view.codex_home,
        environment: view.environment,
        native_subagents: view.native_subagents,
        capabilities: capability_snapshot(None),
    })
    .map_err(internal)
}

fn remove(parsed: &Parsed) -> Result<Value, MachineError> {
    let name = parsed.required_positional(0, "profile name")?;
    let context = context(parsed.path("--workspace")?.as_deref())?;
    let _config_lock = config_lock(&context)?;
    let mut registry = load_registry(&context)?;
    if registry.profiles.remove(&name).is_none() {
        return Err(profile_not_found(&name));
    }
    store_registry(&context, &registry)?;
    Ok(json!({"profile": name, "removed": true}))
}

fn doctor(parsed: &Parsed) -> Result<Value, MachineError> {
    let launch_probe = parsed.flag("--launch-probe");
    let leave_running = parsed.flag("--leave-running");
    if leave_running && !launch_probe {
        return Err(MachineError::invalid_argument(
            "--leave-running",
            "--leave-running requires --launch-probe",
        ));
    }
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = match snapshot_for(&context, &name, &profile) {
        Ok(snapshot) => snapshot,
        Err(error) if error.code == "COMPATIBILITY_REJECTED" => {
            return serde_json::to_value(DoctorResult {
                profile: name,
                compatibility: CompatibilityVerdict::Rejected,
                codex_version: None,
                expected_codex_home: profile.codex_home,
                schema_bundle_sha256: None,
                manifest_sha256: Some(sha256_hex(MANIFEST.as_bytes())),
                server_key: None,
                launch_probe,
                server_started: false,
                server_epoch: None,
                default_model: None,
                models: Vec::new(),
                capabilities: capability_snapshot(None),
                diagnostics: vec![json!({
                    "code": error.code,
                    "message": error.message,
                    "details": error.details,
                })],
            })
            .map_err(internal);
        }
        Err(error) => return Err(error),
    };
    let mut started = false;
    let mut probed_state = None;
    if launch_probe {
        let state_path = profile_state_path(&context, &snapshot);
        probed_state = read_state_if_running(&state_path)?;
        if probed_state.is_none() {
            probed_state = Some(start_snapshot_for_probe(
                &context,
                &snapshot,
                &mut OperatorHandoff::none(),
            )?);
            started = true;
        }
        if started && !leave_running {
            stop_snapshot(&context, &snapshot, false, &mut OperatorHandoff::none())?;
        }
    }
    // Bare doctor never launches a probe, but it still reports the closed
    // capability snapshot from whatever singleton is already recorded as
    // running; a launch-probed snapshot always wins when one was taken.
    let capabilities = match &probed_state {
        Some(state) => capability_snapshot(Some(&state.capabilities)),
        None => {
            let stored = read_state_if_running(&profile_state_path(&context, &snapshot))?;
            capability_snapshot(stored.as_ref().map(|state| &state.capabilities))
        }
    };
    serde_json::to_value(DoctorResult {
        profile: name,
        compatibility: snapshot.compatibility_verdict,
        codex_version: Some(snapshot.codex_version.clone()),
        expected_codex_home: snapshot.canonical_codex_home.clone(),
        schema_bundle_sha256: Some(snapshot.schema_bundle_sha256.clone()),
        manifest_sha256: Some(snapshot.compatibility_manifest_sha256.clone()),
        server_key: Some(snapshot.server_key.clone()),
        launch_probe,
        server_started: launch_probe && (leave_running || !started),
        server_epoch: probed_state.as_ref().map(|state| state.server_epoch),
        default_model: probed_state
            .as_ref()
            .map(|state| state.default_model.clone()),
        models: probed_state
            .as_ref()
            .map_or_else(Vec::new, |state| state.models.clone()),
        capabilities,
        diagnostics: if snapshot.compatibility_verdict == CompatibilityVerdict::Unverified {
            vec![json!({
                "code": "CODEX_VERSION_UNVERIFIED",
                "message": "the compatible newer Codex version is not the tested baseline",
            })]
        } else {
            Vec::new()
        },
    })
    .map_err(internal)
}

fn server_status(parsed: &Parsed) -> Result<Value, MachineError> {
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let state = read_state_if_running(&profile_state_path(&context, &snapshot))?;
    Ok(json!({
        "profile": name,
        "server_key": snapshot.server_key,
        "lifecycle": state.as_ref().map_or("stopped", |value| value.lifecycle.as_str()),
        "state": state,
    }))
}

fn server_start(parsed: &Parsed) -> Result<Value, MachineError> {
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let state = start_snapshot(&context, &snapshot, &mut OperatorHandoff::none())?;
    Ok(json!({"profile": name, "started": true, "state": state}))
}

fn server_stop(parsed: &Parsed, allow_without_operator: bool) -> Result<Value, MachineError> {
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    if !allow_without_operator {
        parsed.precheck_operator("profile.server.stop")?;
    }
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let interrupt = parsed.flag("--interrupt");
    require_interrupt_confirmation(parsed, &name, &snapshot, interrupt)?;
    let mut operator = if allow_without_operator {
        OperatorHandoff::none()
    } else {
        parsed.authorize_operator("profile.server.stop")?
    };
    let stopped = stop_snapshot(&context, &snapshot, interrupt, &mut operator)?;
    Ok(json!({"profile": name, "server_key": snapshot.server_key, "stopped": stopped}))
}

fn server_restart(parsed: &Parsed) -> Result<Value, MachineError> {
    parsed.precheck_operator("profile.server.restart")?;
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let interrupt = parsed.flag("--interrupt");
    require_interrupt_confirmation(parsed, &name, &snapshot, interrupt)?;
    let mut stop_operator = parsed.authorize_operator("profile.server.restart")?;
    stop_snapshot(&context, &snapshot, interrupt, &mut stop_operator)?;
    // Restarting is two operator-authorized effects. The stop handed
    // `operator.lock` off to the home and server locks and then released it
    // for the quiesce, so the start reauthorizes: a credential rotated while
    // the old server was going down must stop the old generation from
    // bringing a new one up.
    let mut start_operator = parsed.authorize_operator("profile.server.restart")?;
    let state = start_snapshot(&context, &snapshot, &mut start_operator)?;
    Ok(json!({"profile": name, "restarted": true, "state": state}))
}

fn require_interrupt_confirmation(
    parsed: &Parsed,
    name: &str,
    snapshot: &ProfileSnapshot,
    interrupt: bool,
) -> Result<(), MachineError> {
    if interrupt {
        let confirmed = parsed.required("--confirm-server-key")?;
        if confirmed != snapshot.server_key {
            return Err(profile_mismatch(
                name,
                "confirm_server_key",
                json!(snapshot.server_key),
                json!(confirmed),
            ));
        }
    }
    Ok(())
}

fn server_migrate(parsed: &Parsed) -> Result<Value, MachineError> {
    parsed.precheck_operator("profile.server.migrate")?;
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let old = parsed.required("--confirm-old-server-key")?;
    let new = parsed.required("--confirm-new-server-key")?;
    require_canonical_server_key("--confirm-old-server-key", &old)?;
    require_canonical_server_key("--confirm-new-server-key", &new)?;
    if new != snapshot.server_key {
        return Err(profile_mismatch(
            &name,
            "confirm_new_server_key",
            json!(snapshot.server_key),
            json!(new),
        ));
    }
    if old == new {
        return Err(profile_mismatch(
            &name,
            "confirm_old_server_key_distinct",
            json!(true),
            json!(false),
        ));
    }
    let old_root = context.application_support_root.join("profiles").join(&old);
    let new_root = profile_root(&context, &snapshot);
    verify_private_directory(&old_root)?;
    secure_dir(&new_root)?;
    let home_root = home_root(&context, &snapshot.canonical_codex_home)?;
    secure_dir(&home_root)?;
    let migration_path = home_root.join("migration.json");

    // Serialize fence creation with both automatic migration and every
    // lifecycle reservation. The operator hold crosses into home.lock before
    // it is released, preserving the global operator -> home -> server order.
    let mut stop_operator = parsed.authorize_operator("profile.server.migrate")?;
    let migration_locks =
        acquire_migration_locks(&home_root, &old_root, &old, &new_root, &snapshot.server_key)?;
    stop_operator.handoff(&migration_locks.home);
    if let Some((migration_id, new_state)) =
        reconcile_blocked_migration(&context, &snapshot, &old, &new, &home_root, &migration_path)?
    {
        drop(migration_locks);
        return Ok(json!({
            "profile": name,
            "migrated": true,
            "migration_id": migration_id,
            "server_key": new,
            "server_epoch": new_state.server_epoch,
        }));
    }
    verify_migration_fence(&migration_path, None, &name, &snapshot.server_key)?;
    let migration_id = Uuid::now_v7();

    // Re-read every source proof under the migration locks. Observations made
    // before this point cannot authorize terminating a later lifetime.
    let old_state = read_state_if_running(&old_root.join("state.json"))?
        .ok_or_else(|| profile_mismatch(&name, "old_server_running", json!(true), json!(false)))?;
    if old_state.server_key != old {
        return Err(profile_mismatch(
            &name,
            "old_server_key",
            json!(old),
            json!(old_state.server_key),
        ));
    }
    if old_state.snapshot.canonical_codex_home != snapshot.canonical_codex_home {
        return Err(profile_mismatch(
            &name,
            "canonical_codex_home",
            json!(snapshot.canonical_codex_home),
            json!(old_state.snapshot.canonical_codex_home),
        ));
    }
    let active = read_home_active(&home_root.join("active.json"))?;
    verify_stop_home_active(&old_state.snapshot, &old_state, active.as_ref())?;
    let old_scope = MembershipScope {
        profile: name.clone(),
        server_key: old.clone(),
    };
    let old_records = replay_membership(&old_root.join("membership.jsonl"), &old_scope)?;
    let old_index = derive_membership_index(&old_root, &old_records)?;
    verify_membership_index(&old_root, &old_index, &old_scope)?;
    let orphans = membership_orphans(&context, &old_state.snapshot, &old_index)?;
    if !orphans.is_empty() {
        return Err(old_scope.incomplete(format!(
            "migration source membership has {} orphan members",
            orphans.len()
        )));
    }
    if !old_index.active_members.is_empty() && !parsed.flag("--interrupt") {
        return Err(profile_membership_incomplete(
            &name,
            &old,
            "migration with live members requires explicit --interrupt",
        ));
    }
    let mut migration =
        migration_record(migration_id, &old_state, &snapshot, "operator_authorized");
    atomic_replace(
        &migration_path,
        &serde_json::to_vec_pretty(&migration).map_err(internal)?,
    )?;
    drop(migration_locks);
    let new_state = execute_migration(
        MigrationExecution {
            migration_id,
            context: &context,
            new_snapshot: &snapshot,
            old_state: &old_state,
            migration_path: &migration_path,
            migration: &mut migration,
            interrupt: parsed.flag("--interrupt"),
        },
        &mut stop_operator,
        || parsed.authorize_operator("profile.server.migrate"),
    )?;
    Ok(json!({
        "profile": name,
        "migrated": true,
        "migration_id": migration_id,
        "server_key": new,
        "server_epoch": new_state.server_epoch,
    }))
}

fn is_canonical_server_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn require_canonical_server_key(argument: &str, value: &str) -> Result<(), MachineError> {
    if is_canonical_server_key(value) {
        return Ok(());
    }
    Err(MachineError::invalid_argument(
        argument,
        "server key must be exactly 64 lowercase hexadecimal characters",
    ))
}

fn require_canonical_persisted_server_key(value: &str) -> Result<(), MachineError> {
    if is_canonical_server_key(value) {
        Ok(())
    } else {
        Err(transport("migration server identity is invalid"))
    }
}

struct MigrationLocks {
    home: File,
    first_server: Option<File>,
    second_server: Option<File>,
}

impl Drop for MigrationLocks {
    fn drop(&mut self) {
        drop(self.second_server.take());
        drop(self.first_server.take());
        // `home` is dropped after this method returns.
    }
}

fn acquire_migration_locks(
    home_root: &Path,
    old_root: &Path,
    old_key: &str,
    new_root: &Path,
    new_key: &str,
) -> Result<MigrationLocks, MachineError> {
    let home = lock_file(&home_root.join("home.lock"))?;
    let (first_root, second_root) = if old_key < new_key {
        (old_root, new_root)
    } else {
        (new_root, old_root)
    };
    Ok(MigrationLocks {
        home,
        first_server: Some(lock_file(&first_root.join("server.lock"))?),
        second_server: Some(lock_file(&second_root.join("server.lock"))?),
    })
}

fn migration_record(
    migration_id: Uuid,
    old_state: &ServerState,
    new_snapshot: &ProfileSnapshot,
    authority: &str,
) -> Value {
    json!({
        "schema_version": 1,
        "migration_id": migration_id,
        "authority": authority,
        "old_server_key": old_state.server_key,
        "new_server_key": new_snapshot.server_key,
        "old_epoch": old_state.server_epoch,
        "new_epoch": null,
        "old_version": old_state.snapshot.codex_version,
        "new_version": new_snapshot.codex_version,
        "old_executable_sha256": old_state.snapshot.executable_identity.sha256,
        "new_executable_sha256": new_snapshot.executable_identity.sha256,
        "old_schema_sha256": old_state.snapshot.schema_bundle_sha256,
        "new_schema_sha256": new_snapshot.schema_bundle_sha256,
        "old_manifest_sha256": old_state.snapshot.compatibility_manifest_sha256,
        "new_manifest_sha256": new_snapshot.compatibility_manifest_sha256,
        "phase": "prepared",
    })
}

struct MigrationExecution<'a> {
    migration_id: Uuid,
    context: &'a Context,
    new_snapshot: &'a ProfileSnapshot,
    old_state: &'a ServerState,
    migration_path: &'a Path,
    migration: &'a mut Value,
    interrupt: bool,
}

fn execute_migration(
    execution: MigrationExecution<'_>,
    stop_operator: &mut OperatorHandoff,
    authorize_start: impl FnOnce() -> Result<OperatorHandoff, MachineError>,
) -> Result<ServerState, MachineError> {
    let MigrationExecution {
        migration_id,
        context,
        new_snapshot,
        old_state,
        migration_path,
        migration,
        interrupt,
    } = execution;
    if let Err(failure) = stop_snapshot_for_migration(
        context,
        &old_state.snapshot,
        migration_id,
        interrupt,
        stop_operator,
    ) {
        let failure = *failure;
        if failure.process_stopped {
            return compensate_stopped_migration(
                context,
                old_state,
                migration_id,
                migration_path,
                migration,
                failure.reservation.as_ref(),
                failure.error,
            );
        }
        let error = failure.error;
        migration["phase"] = Value::String("rolled_back".to_owned());
        migration["failure"] = Value::String(error.code.clone());
        persist_migration(migration_path, migration)?;
        return Err(error);
    }
    migration["phase"] = Value::String("applying".to_owned());
    if let Err(error) = persist_migration(migration_path, migration) {
        return compensate_stopped_migration(
            context,
            old_state,
            migration_id,
            migration_path,
            migration,
            None,
            error,
        );
    }
    let mut start_operator = match authorize_start() {
        Ok(operator) => operator,
        Err(error) => {
            return compensate_stopped_migration(
                context,
                old_state,
                migration_id,
                migration_path,
                migration,
                None,
                error,
            );
        }
    };
    let new_state = match start_snapshot_for_migration(
        context,
        new_snapshot,
        migration_id,
        &mut start_operator,
    ) {
        Ok(state) => state,
        Err(error) => {
            return compensate_stopped_migration(
                context,
                old_state,
                migration_id,
                migration_path,
                migration,
                None,
                error,
            );
        }
    };
    migration["new_epoch"] = Value::from(new_state.server_epoch);
    migration["phase"] = Value::String("committed".to_owned());
    if let Err(error) = persist_migration_attempts(migration_path, migration, 6) {
        migration["phase"] = Value::String("migration_blocked".to_owned());
        migration["failure"] = Value::String(error.code.clone());
        persist_migration(migration_path, migration)?;
        return Err(error);
    }
    Ok(new_state)
}

fn persist_migration(path: &Path, migration: &Value) -> Result<(), MachineError> {
    persist_migration_attempts(path, migration, 3)
}

fn persist_migration_attempts(
    path: &Path,
    migration: &Value,
    attempts: usize,
) -> Result<(), MachineError> {
    persist_migration_with(path, migration, attempts, atomic_replace)
}

fn persist_migration_with(
    path: &Path,
    migration: &Value,
    attempts: usize,
    mut write: impl FnMut(&Path, &[u8]) -> Result<(), MachineError>,
) -> Result<(), MachineError> {
    let bytes = serde_json::to_vec_pretty(migration).map_err(internal)?;
    let mut last_error = None;
    for _ in 0..attempts {
        match write(path, &bytes) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.expect("migration persistence attempt budget must be non-zero"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationPhase {
    Prepared,
    Applying,
    Blocked,
    Committed,
    RolledBack,
}

impl MigrationPhase {
    fn parse(value: &str) -> Result<Self, MachineError> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "applying" => Ok(Self::Applying),
            "migration_blocked" => Ok(Self::Blocked),
            "committed" => Ok(Self::Committed),
            "rolled_back" => Ok(Self::RolledBack),
            _ => Err(transport("migration transaction phase is invalid")),
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::RolledBack)
    }
}

#[derive(Debug)]
struct ParsedMigrationRecord {
    migration_id: Uuid,
    phase: MigrationPhase,
    old_server_key: String,
    new_server_key: String,
}

fn read_migration_record(path: &Path) -> Result<(Value, ParsedMigrationRecord), MachineError> {
    verify_private_regular_file(path, 0o600)?;
    let value: Value = serde_json::from_slice(&fs::read(path).map_err(io_error)?)
        .map_err(|_| transport("migration transaction is invalid"))?;
    let migration_id = value["migration_id"]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| transport("migration transaction identity is invalid"))?;
    let phase = MigrationPhase::parse(
        value["phase"]
            .as_str()
            .ok_or_else(|| transport("migration transaction phase is invalid"))?,
    )?;
    let old_server_key = value["old_server_key"]
        .as_str()
        .ok_or_else(|| transport("migration old server identity is invalid"))?
        .to_owned();
    let new_server_key = value["new_server_key"]
        .as_str()
        .ok_or_else(|| transport("migration new server identity is invalid"))?
        .to_owned();
    require_canonical_persisted_server_key(&old_server_key)?;
    require_canonical_persisted_server_key(&new_server_key)?;
    Ok((
        value,
        ParsedMigrationRecord {
            migration_id,
            phase,
            old_server_key,
            new_server_key,
        },
    ))
}

/// Operator-authorized recovery for an unresolved migration fence. The caller
/// holds home.lock and both ordered server locks, so the ready generation
/// proved here cannot change before the terminal phase is persisted.
fn reconcile_blocked_migration(
    context: &Context,
    new_snapshot: &ProfileSnapshot,
    confirmed_old_key: &str,
    confirmed_new_key: &str,
    home_root: &Path,
    migration_path: &Path,
) -> Result<Option<(Uuid, ServerState)>, MachineError> {
    if !migration_path.exists() {
        return Ok(None);
    }
    let (mut migration, parsed) = read_migration_record(migration_path)?;
    if parsed.phase.is_terminal() {
        return Ok(None);
    }
    if parsed.phase != MigrationPhase::Blocked {
        return Ok(None);
    }
    if parsed.old_server_key != confirmed_old_key || parsed.new_server_key != confirmed_new_key {
        return Ok(None);
    }

    let Some(active) = read_home_active(&home_root.join("active.json"))? else {
        return Ok(None);
    };
    if active.lifecycle != "ready" || active.transition_token.is_some() {
        return Ok(None);
    }
    if active.server_key == confirmed_new_key {
        let state = read_state_if_running(
            &context
                .application_support_root
                .join("profiles")
                .join(confirmed_new_key)
                .join("state.json"),
        )?
        .ok_or_else(|| {
            profile_mismatch(
                &new_snapshot.profile_name,
                "migration_recovery_new_ready",
                json!(true),
                json!(false),
            )
        })?;
        let state = attach_running(new_snapshot, &state, Some(&active))?;
        migration["new_epoch"] = Value::from(state.server_epoch);
        migration["phase"] = Value::String("committed".to_owned());
        migration["failure"] = Value::Null;
        persist_migration(migration_path, &migration)?;
        return Ok(Some((parsed.migration_id, state)));
    }
    if active.server_key == confirmed_old_key {
        let state = read_state_if_running(
            &context
                .application_support_root
                .join("profiles")
                .join(confirmed_old_key)
                .join("state.json"),
        )?
        .ok_or_else(|| {
            profile_mismatch(
                &new_snapshot.profile_name,
                "migration_recovery_old_ready",
                json!(true),
                json!(false),
            )
        })?;
        attach_running(&state.snapshot, &state, Some(&active))?;
        migration["phase"] = Value::String("rolled_back".to_owned());
        migration["failure"] = Value::String("OPERATOR_RECONCILED".to_owned());
        persist_migration(migration_path, &migration)?;
    }
    Ok(None)
}

/// Every error after the old lifetime has stopped takes the same compensating
/// path. Restoration is migration-owned and therefore may cross the durable
/// fence without fresh operator authority; the final phase records whether
/// that restoration was actually proved.
fn compensate_stopped_migration(
    context: &Context,
    old_state: &ServerState,
    migration_id: Uuid,
    migration_path: &Path,
    migration: &mut Value,
    stop_reservation: Option<&StopReservation>,
    error: MachineError,
) -> Result<ServerState, MachineError> {
    if let Some(reservation) = stop_reservation
        && settle_terminated_migration_stop(context, &old_state.snapshot, reservation).is_err()
    {
        migration["phase"] = Value::String("migration_blocked".to_owned());
        migration["failure"] = Value::String(error.code.clone());
        persist_migration(migration_path, migration)?;
        return Err(error);
    }
    let restored = start_snapshot_for_migration(
        context,
        &old_state.snapshot,
        migration_id,
        &mut OperatorHandoff::none(),
    );
    migration["phase"] = Value::String(
        if restored.is_ok() {
            "rolled_back"
        } else {
            "migration_blocked"
        }
        .to_owned(),
    );
    migration["failure"] = Value::String(error.code.clone());
    persist_migration(migration_path, migration)?;
    Err(error)
}

/// Replaces a different live contract only when its complete durable
/// membership proves that no Run can observe the stop. Ordinary Codex clients
/// are outside this registry and are never enumerated or signalled here.
fn automatic_quiescent_migration(
    context: &Context,
    new_snapshot: &ProfileSnapshot,
    old_server_key: &str,
) -> Result<ServerState, MachineError> {
    require_canonical_persisted_server_key(old_server_key)?;
    require_canonical_persisted_server_key(&new_snapshot.server_key)?;
    let home = home_root(context, &new_snapshot.canonical_codex_home)?;
    let active_path = home.join("active.json");
    let migration_path = home.join("migration.json");
    let old_root = context
        .application_support_root
        .join("profiles")
        .join(old_server_key);
    let new_root = profile_root(context, new_snapshot);
    verify_private_directory(&old_root)?;
    verify_private_directory(&new_root)?;

    let migration_locks = acquire_migration_locks(
        &home,
        &old_root,
        old_server_key,
        &new_root,
        &new_snapshot.server_key,
    )?;
    verify_migration_fence(
        &migration_path,
        None,
        &new_snapshot.profile_name,
        &new_snapshot.server_key,
    )?;

    let active = read_home_active(&active_path)?.ok_or_else(|| {
        profile_mismatch(
            &new_snapshot.profile_name,
            "home_active_contract",
            json!({"server_key": old_server_key, "lifecycle": "ready"}),
            Value::Null,
        )
    })?;
    if active.server_key == new_snapshot.server_key
        && active.lifecycle == "ready"
        && active.transition_token.is_none()
    {
        let new_state = read_state_if_running(&new_root.join("state.json"))?.ok_or_else(|| {
            profile_mismatch(
                &new_snapshot.profile_name,
                "automatic_rollover_requested_server_running",
                json!(true),
                json!(false),
            )
        })?;
        return attach_running(new_snapshot, &new_state, Some(&active));
    }
    if active.server_key != old_server_key
        || active.lifecycle != "ready"
        || active.transition_token.is_some()
    {
        return Err(profile_server_busy(
            &new_snapshot.profile_name,
            &active.server_key,
            format!(
                "the CODEX_HOME contract changed while preparing automatic rollover ({})",
                active.lifecycle
            ),
        ));
    }
    let old_state = read_state_if_running(&old_root.join("state.json"))?.ok_or_else(|| {
        profile_mismatch(
            &new_snapshot.profile_name,
            "old_server_running",
            json!(true),
            json!(false),
        )
    })?;
    if old_state.server_key != old_server_key
        || old_state.snapshot.canonical_codex_home != new_snapshot.canonical_codex_home
        || active.server_epoch != old_state.server_epoch
        || active.pid != Some(old_state.pid)
    {
        return Err(profile_mismatch(
            &new_snapshot.profile_name,
            "automatic_rollover_source",
            json!({
                "canonical_codex_home": new_snapshot.canonical_codex_home,
                "server_key": old_server_key,
                "server_epoch": active.server_epoch,
                "pid": active.pid,
            }),
            json!({
                "canonical_codex_home": old_state.snapshot.canonical_codex_home,
                "server_key": old_state.server_key,
                "server_epoch": old_state.server_epoch,
                "pid": old_state.pid,
            }),
        ));
    }
    verify_recorded_socket(&old_state.snapshot.profile_name, &old_state)?;

    let old_scope = MembershipScope {
        profile: old_state.snapshot.profile_name.clone(),
        server_key: old_server_key.to_owned(),
    };
    let old_records = replay_membership(&old_root.join("membership.jsonl"), &old_scope)?;
    if old_records.last().map_or(0, |record| record.revision) != old_state.membership_revision {
        return Err(old_scope.incomplete("membership journal and server state revisions differ"));
    }
    let old_index = derive_membership_index(&old_root, &old_records)?;
    verify_membership_index(&old_root, &old_index, &old_scope)?;
    let orphans = membership_orphans(context, &old_state.snapshot, &old_index)?;
    if !orphans.is_empty() {
        return Err(old_scope.incomplete(format!(
            "automatic rollover source has {} orphan members",
            orphans.len()
        )));
    }
    let members = live_member_evidence(&old_index);
    if !members.is_empty() {
        return Err(old_scope.incomplete(format!(
            "automatic rollover would interrupt {} live run member(s); use operator-authorized profile server migrate",
            members.len()
        )));
    }

    let migration_id = Uuid::now_v7();
    let mut migration = migration_record(
        migration_id,
        &old_state,
        new_snapshot,
        "automatic_quiescent",
    );
    atomic_replace(
        &migration_path,
        &serde_json::to_vec_pretty(&migration).map_err(internal)?,
    )?;
    drop(migration_locks);

    execute_migration(
        MigrationExecution {
            migration_id,
            context,
            new_snapshot,
            old_state: &old_state,
            migration_path: &migration_path,
            migration: &mut migration,
            interrupt: false,
        },
        &mut OperatorHandoff::none(),
        || Ok(OperatorHandoff::none()),
    )
}

fn membership_verify(parsed: &Parsed) -> Result<Value, MachineError> {
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let root = profile_root(&context, &snapshot);
    let scope = MembershipScope {
        profile: name.clone(),
        server_key: snapshot.server_key.clone(),
    };
    let records = replay_membership(&root.join("membership.jsonl"), &scope)?;
    let index = derive_membership_index(&root, &records)?;
    verify_membership_index(&root, &index, &scope)?;
    let orphans = membership_orphans(&context, &snapshot, &index)?;
    Ok(json!({
        "profile": name,
        "server_key": snapshot.server_key,
        "complete": orphans.is_empty(),
        "revision": records.last().map_or(0, |record| record.revision),
        "records": records.len(),
        "orphans": orphans,
    }))
}

fn membership_tombstone(parsed: &Parsed) -> Result<Value, MachineError> {
    parsed.precheck_operator("profile.membership.tombstone_orphan")?;
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let confirmed_server_key = parsed.required("--confirm-server-key")?;
    if confirmed_server_key != snapshot.server_key {
        return Err(profile_mismatch(
            &name,
            "server_key",
            json!(snapshot.server_key),
            json!(confirmed_server_key),
        ));
    }
    let workspace = parsed.required("--confirm-workspace-id")?;
    let run = parsed.required("--confirm-run-id")?;
    let root = profile_root(&context, &snapshot);
    let scope = MembershipScope {
        profile: name.clone(),
        server_key: snapshot.server_key.clone(),
    };
    let records = replay_membership(&root.join("membership.jsonl"), &scope)?;
    let index = derive_membership_index(&root, &records)?;
    let orphans = membership_orphans(&context, &snapshot, &index)?;
    if !orphans
        .iter()
        .any(|value| value["workspace_id"] == workspace && value["run_id"] == run)
    {
        return Err(profile_mismatch(
            &name,
            "confirmed_orphan_member",
            json!({"workspace_id": workspace, "run_id": run}),
            json!(orphans),
        ));
    }
    // Membership repair acquires no lock below `operator.lock`, so the hold
    // taken here has nothing to hand off to and instead spans the tombstone
    // append and the revision commit that follows it.
    let operator = parsed.authorize_operator("profile.membership.tombstone_orphan")?;
    let revision = append_membership(
        &root.join("membership.jsonl"),
        "tombstone_orphan",
        &workspace,
        Some(run),
        &scope,
    )?;
    let state_path = root.join("state.json");
    if let Some(mut state) = read_state(&state_path)? {
        state.membership_revision = revision;
        atomic_replace(
            &state_path,
            &serde_json::to_vec_pretty(&state).map_err(internal)?,
        )?;
    }
    operator.release();
    Ok(json!({"profile": name, "revision": revision, "tombstoned": true}))
}

fn state_reset(parsed: &Parsed) -> Result<Value, MachineError> {
    parsed.precheck_operator("profile.state.reset")?;
    if !parsed.flag("--require-server-absence") {
        return Err(MachineError::invalid_argument(
            "--require-server-absence",
            "state reset requires explicit server absence proof",
        ));
    }
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let confirm_server_key = parsed.required("--confirm-server-key")?;
    if confirm_server_key != snapshot.server_key {
        return Err(profile_mismatch(
            &name,
            "confirm_server_key",
            json!(snapshot.server_key),
            json!(confirm_server_key),
        ));
    }
    if !server_lifetime_absent(&context, &snapshot.server_key)? {
        return Err(profile_server_busy(
            &name,
            &snapshot.server_key,
            "profile process, drainer, or socket absence is not proven",
        ));
    }
    // The absence proof above must not run under any filesystem lock, so the
    // authorizing hold is taken only once it is behind us and stays live
    // across the state removal until the home lock replaces it.
    let mut operator = parsed.authorize_operator("profile.state.reset")?;
    let state_path = profile_state_path(&context, &snapshot);
    if state_path.exists() {
        fs::remove_file(&state_path).map_err(io_error)?;
        sync_parent(&state_path)?;
    }
    let home = home_root(&context, &snapshot.canonical_codex_home)?;
    let mut migration_repaired = false;
    if home.exists() {
        let home_lock = lock_file(&home.join("home.lock"))?;
        operator.handoff(&home_lock);
        clear_home_active(
            &snapshot.profile_name,
            &home.join("active.json"),
            &snapshot.server_key,
        )?;
        migration_repaired = repair_stale_migration_fence(&context, &home, &snapshot.server_key)?;
    }
    operator.release();
    Ok(json!({"profile": name, "reset": true, "migration_repaired": migration_repaired}))
}

fn diagnostics(parsed: &Parsed, events: bool) -> Result<Value, MachineError> {
    if parsed.flag("--follow") {
        return Err(MachineError::invalid_argument(
            "--follow",
            "follow delivery is owned by TASK-028 status and change observation",
        ));
    }
    let projection = parsed
        .values
        .get("--projection")
        .and_then(|values| values.last())
        .map_or("minimal", String::as_str);
    if projection == "operational" {
        // A projection read causes no durable change, so there is no effect
        // for a retained hold to fence: the check stands on its own here.
        parsed.precheck_operator(if events {
            "profile.events"
        } else {
            "profile.diagnostics.list"
        })?;
    } else if projection != "minimal" {
        return Err(MachineError::invalid_argument(
            "--projection",
            "projection must be minimal or operational",
        ));
    }
    let after = parsed
        .values
        .get("--after")
        .and_then(|values| values.last())
        .map_or(Ok(0_usize), |value| value.parse::<usize>())
        .map_err(|_| MachineError::invalid_argument("--after", "cursor must be an integer"))?;
    let limit = parsed
        .values
        .get("--limit")
        .and_then(|values| values.last())
        .map_or(Ok(100_usize), |value| value.parse::<usize>())
        .map_err(|_| MachineError::invalid_argument("--limit", "limit must be an integer"))?;
    if limit == 0 || limit > 1000 {
        return Err(MachineError::invalid_argument(
            "--limit",
            "limit must be between 1 and 1000",
        ));
    }
    let (context, name, profile, _) = selected_profile_from(parsed)?;
    let snapshot = snapshot_for(&context, &name, &profile)?;
    let path = profile_root(&context, &snapshot).join("diagnostics.jsonl");
    let bytes = if path.exists() {
        fs::read(&path).map_err(io_error)?
    } else {
        Vec::new()
    };
    if bytes.len() as u64 > MAX_DIAGNOSTIC_BYTES {
        return Err(transport("profile diagnostic journal exceeds 8 MiB"));
    }
    let total = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count();
    let values = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .skip(after)
        .take(limit)
        .map(|line| {
            serde_json::from_slice::<Value>(line)
                .map_err(internal)
                .map(|record| project_diagnostic(record, projection))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = after + values.len();
    Ok(json!({
        "profile": name,
        "server_key": snapshot.server_key,
        "projection": projection,
        "events": events,
        "items": values,
        "next_cursor": next_cursor,
        "has_more": total > next_cursor,
    }))
}

fn selected_profile_from(
    parsed: &Parsed,
) -> Result<(Context, String, RuntimeProfile, Parsed), MachineError> {
    let name = parsed.required_positional(0, "profile name")?;
    let context = context(parsed.path("--workspace")?.as_deref())?;
    let registry = load_registry(&context)?;
    let profile = registry
        .profiles
        .get(&name)
        .cloned()
        .ok_or_else(|| profile_not_found(&name))?;
    Ok((context, name, profile, parsed.clone()))
}

fn context(workspace: Option<&Path>) -> Result<Context, MachineError> {
    let service = WorkspaceService::system()?;
    let view = service.discover(workspace)?;
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MachineError::runtime_path_invalid("HOME", "HOME is not set"))?;
    let application_support_root = fs::canonicalize(home)
        .map_err(io_error)?
        .join("Library/Application Support/Dolgorae");
    let state_root = application_support_root
        .join("workspaces")
        .join(&view.workspace_id);
    Ok(Context {
        workspace_id: view.workspace_id,
        registry_path: state_root.join("local.yaml"),
        application_support_root,
    })
}

fn load_registry(context: &Context) -> Result<LocalProfileRegistry, MachineError> {
    verify_private_regular_file(&context.registry_path, 0o600)?;
    parse_local_profiles(&context.registry_path)
}

fn store_registry(context: &Context, registry: &LocalProfileRegistry) -> Result<(), MachineError> {
    let bytes = serde_yaml_ng::to_string(registry)
        .map_err(internal)?
        .into_bytes();
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(MachineError::profile_config_invalid(
            &context.registry_path,
            "profile registry exceeds 1 MiB",
        ));
    }
    let parent = context
        .registry_path
        .parent()
        .ok_or_else(|| transport("profile registry has no parent"))?;
    verify_private_regular_file(&context.registry_path, 0o600)?;
    let temporary = parent.join(format!(".local-{}.yaml", Uuid::now_v7()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(io_error)?;
    file.write_all(&bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    if let Err(error) = parse_local_profiles(&temporary) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, &context.registry_path).map_err(io_error)?;
    sync_parent(&context.registry_path)
}

fn config_lock(context: &Context) -> Result<File, MachineError> {
    let parent = context
        .registry_path
        .parent()
        .ok_or_else(|| transport("profile registry has no parent"))?;
    verify_private_directory(parent)?;
    lock_file(&parent.join("local.lock"))
}

fn profile_view(name: &str, profile: &RuntimeProfile) -> ProfileView {
    ProfileView {
        name: name.to_owned(),
        argv: profile.argv.clone(),
        codex_home: profile.codex_home.clone(),
        environment: profile.environment.clone(),
        native_subagents: "enabled".to_owned(),
    }
}

fn snapshot_for(
    context: &Context,
    name: &str,
    profile: &RuntimeProfile,
) -> Result<ProfileSnapshot, MachineError> {
    let executable = Path::new(&profile.argv[0]);
    let canonical_executable = fs::canonicalize(executable).map_err(|error| {
        MachineError::profile_config_invalid(&context.registry_path, error.to_string())
    })?;
    let metadata = fs::metadata(&canonical_executable).map_err(io_error)?;
    let executable_identity = ExecutableIdentity {
        resolved_path: path_utf8(&canonical_executable)?.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        sha256: file_sha256(&canonical_executable)?,
    };
    let canonical_codex_home = fs::canonicalize(&profile.codex_home).map_err(|error| {
        MachineError::profile_config_invalid(&context.registry_path, format!("CODEX_HOME: {error}"))
    })?;
    let canonical_codex_home = path_utf8(&canonical_codex_home)?.to_owned();
    let version = codex_version(name, profile, &canonical_executable)?;
    let schema_root = generate_schema(name, profile, &canonical_executable)?;
    let result = (|| {
        compare_required_subset(name, &schema_root.join("stable"))?;
        let schema_bundle_sha256 = directory_sha256(name, &schema_root.join("stable"))?;
        verify_exact_bundle_digests(name, &schema_root, &version, &schema_bundle_sha256)?;
        let compatibility_manifest_sha256 = sha256_hex(MANIFEST.as_bytes());
        let verdict = version_verdict(name, &version)?;
        let normalized_argv = normalized_argv(profile, &canonical_executable)?;
        let mut sanitized_environment = prepared_environment(profile)?;
        sanitized_environment.insert("CODEX_HOME".to_owned(), canonical_codex_home.clone());
        let enabled_features = vec!["multi_agent".to_owned()];
        let disabled_features = Vec::new();
        let configuration = configuration_snapshot(name, profile, &canonical_codex_home)?;
        let process_static_configuration = configuration.launch;
        let initial_configuration_observation = configuration.observation;
        let launch_contract = json!({
            "schema_version": 1,
            "canonical_codex_home": canonical_codex_home,
            "normalized_argv": normalized_argv,
            "launch_cwd_policy": "profile_state_directory_v1",
            "launch_mode": "app_server_unix_socket_v1",
            "sanitized_environment": sanitized_environment,
            "executable_identity": executable_identity,
            "process_static_configuration": process_static_configuration,
            "codex_version": version,
            "app_server_schema_sha256": schema_bundle_sha256,
            "compatibility_manifest_sha256": compatibility_manifest_sha256,
            "enabled_features": enabled_features,
            "disabled_features": disabled_features,
        });
        let launch_contract_sha256 = canonical_sha256(&launch_contract)?;
        let server_key =
            domain_separated_sha256(b"dolgorae-profile-server-key-v1\0", &launch_contract)?;
        let derived_launch_cwd = context
            .application_support_root
            .join("profiles")
            .join(&server_key);
        Ok(ProfileSnapshot {
            schema_version: 1,
            profile_name: name.to_owned(),
            canonical_codex_home,
            normalized_argv,
            launch_cwd_policy: "profile_state_directory_v1".to_owned(),
            derived_launch_cwd: path_utf8(&derived_launch_cwd)?.to_owned(),
            sanitized_environment,
            enabled_features,
            disabled_features,
            process_static_configuration,
            initial_configuration_observation,
            executable_identity,
            codex_version: version,
            schema_bundle_sha256,
            compatibility_manifest_sha256,
            launch_contract_sha256,
            compatibility_verdict: verdict,
            server_key,
        })
    })();
    let _ = fs::remove_dir_all(&schema_root);
    result
}

fn configuration_snapshot(
    profile_name: &str,
    profile: &RuntimeProfile,
    canonical_codex_home: &str,
) -> Result<ConfigurationSnapshot, MachineError> {
    let manifest: Value = serde_json::from_str(MANIFEST).map_err(internal)?;
    let classes = manifest["profile_launch"]["configuration_fields"]
        .as_object()
        .ok_or_else(|| {
            compatibility(
                profile_name,
                "profile_configuration_classification",
                "configuration classification manifest is missing",
            )
        })?;
    let mut classification = BTreeMap::<String, String>::new();
    for category in [
        "process_static",
        "operator_migratable",
        "runtime_mutable",
        "ignored",
    ] {
        let names = classes[category].as_array().ok_or_else(|| {
            compatibility(
                profile_name,
                "profile_configuration_classification",
                "configuration classification category is invalid",
            )
        })?;
        for name in names {
            let name = name.as_str().ok_or_else(|| {
                compatibility(
                    profile_name,
                    "profile_configuration_classification",
                    "configuration field name is invalid",
                )
            })?;
            if classification
                .insert(name.to_owned(), category.to_owned())
                .is_some()
            {
                return Err(compatibility(
                    profile_name,
                    "profile_configuration_classification",
                    "configuration field is classified twice",
                ));
            }
        }
    }
    let home = Path::new(canonical_codex_home);
    let mut effective = read_toml_table_if_present(profile_name, &home.join("config.toml"))?;
    let selected = selected_codex_profile(&profile.argv);
    if let Some(selected) = selected {
        if let Some(profiles) = effective.remove("profiles")
            && let Some(table) = profiles
                .as_table()
                .and_then(|profiles| profiles.get(selected))
                .and_then(toml::Value::as_table)
        {
            for (key, value) in table {
                effective.insert(key.clone(), value.clone());
            }
        }
        let selected_path = home.join(format!("{selected}.config.toml"));
        for (key, value) in read_toml_table_if_present(profile_name, &selected_path)? {
            effective.insert(key, value);
        }
    }
    for name in effective.keys() {
        if name.contains("include") || !classification.contains_key(name) {
            return Err(compatibility(
                profile_name,
                "profile_configuration_classification",
                format!("configuration field {name:?} has no closed classification"),
            ));
        }
    }
    let mut launch = BTreeMap::new();
    let mut observation = BTreeMap::new();
    for (name, category) in classification {
        let value = effective
            .get(&name)
            .map(toml_to_json)
            .transpose()?
            .unwrap_or(Value::Null);
        match category.as_str() {
            "process_static" | "operator_migratable" => {
                launch.insert(name, value);
            }
            "runtime_mutable" | "ignored" => {
                observation.insert(name, value);
            }
            _ => {
                return Err(compatibility(
                    profile_name,
                    "profile_configuration_classification",
                    "unknown configuration classification",
                ));
            }
        }
    }
    Ok(ConfigurationSnapshot {
        launch,
        observation,
    })
}

fn read_toml_table_if_present(
    profile_name: &str,
    path: &Path,
) -> Result<toml::map::Map<String, toml::Value>, MachineError> {
    if !path.exists() {
        return Ok(toml::map::Map::new());
    }
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > MAX_REGISTRY_BYTES
    {
        return Err(compatibility(
            profile_name,
            "profile_configuration_file",
            format!(
                "configuration input is not a bounded regular file: {}",
                path.display()
            ),
        ));
    }
    let text = fs::read_to_string(path).map_err(|_| {
        compatibility(
            profile_name,
            "profile_configuration_file",
            format!("configuration input is unreadable: {}", path.display()),
        )
    })?;
    text.parse::<toml::Table>().map_err(|_| {
        compatibility(
            profile_name,
            "profile_configuration_file",
            format!("configuration input is invalid: {}", path.display()),
        )
    })
}

fn selected_codex_profile(argv: &[String]) -> Option<&str> {
    argv.windows(2)
        .find(|pair| pair[0] == "--profile")
        .map(|pair| pair[1].as_str())
}

fn toml_to_json(value: &toml::Value) -> Result<Value, MachineError> {
    serde_json::to_value(value).map_err(internal)
}

fn codex_version(
    profile_name: &str,
    profile: &RuntimeProfile,
    executable: &Path,
) -> Result<String, MachineError> {
    let mut command = Command::new(executable);
    command.arg("--version");
    apply_environment(&mut command, &prepared_environment(profile)?);
    let output = command.output().map_err(transport)?;
    if !output.status.success() || output.stdout.len() > 4096 {
        return Err(compatibility(
            profile_name,
            "codex_version",
            "Codex --version failed or exceeded its bound",
        ));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| compatibility(profile_name, "codex_version", "Codex version is not UTF-8"))?;
    let version = text.split_whitespace().last().unwrap_or_default();
    if version.is_empty() {
        return Err(compatibility(
            profile_name,
            "codex_version",
            "Codex version is missing",
        ));
    }
    Ok(version.to_owned())
}

fn version_verdict(
    profile_name: &str,
    version: &str,
) -> Result<CompatibilityVerdict, MachineError> {
    let actual = parse_version(profile_name, version)?;
    let supported = parse_version(profile_name, SUPPORTED_CODEX_VERSION)?;
    if actual < supported {
        return Err(compatibility(
            profile_name,
            "codex_version",
            "Codex version is older than the supported baseline",
        ));
    }
    Ok(if actual == supported {
        CompatibilityVerdict::Tested
    } else {
        CompatibilityVerdict::Unverified
    })
}

fn parse_version(profile_name: &str, value: &str) -> Result<(u64, u64, u64), MachineError> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(compatibility(
            profile_name,
            "codex_version",
            "Codex version is not semantic x.y.z",
        ));
    }
    Ok((
        parts[0].parse().map_err(|_| {
            compatibility(profile_name, "codex_version", "invalid Codex major version")
        })?,
        parts[1].parse().map_err(|_| {
            compatibility(profile_name, "codex_version", "invalid Codex minor version")
        })?,
        parts[2].parse().map_err(|_| {
            compatibility(profile_name, "codex_version", "invalid Codex patch version")
        })?,
    ))
}

fn generate_schema(
    profile_name: &str,
    profile: &RuntimeProfile,
    executable: &Path,
) -> Result<PathBuf, MachineError> {
    let root = std::env::temp_dir().join(format!("dolgorae-schema-{}", Uuid::now_v7()));
    secure_dir(&root)?;
    let stable = root.join("stable");
    let experimental = root.join("experimental");
    for (destination, include_experimental) in [(&stable, false), (&experimental, true)] {
        let mut command = Command::new(executable);
        for argument in profile.argv.iter().skip(1) {
            command.arg(argument);
        }
        command.args([
            "--enable",
            "multi_agent",
            "app-server",
            "generate-json-schema",
            "--out",
        ]);
        command.arg(destination);
        if include_experimental {
            command.arg("--experimental");
        }
        apply_environment(&mut command, &prepared_environment(profile)?);
        let status = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(transport)?;
        if !status.success() {
            let _ = fs::remove_dir_all(&root);
            return Err(compatibility(
                profile_name,
                "codex_schema_generation",
                "Codex schema generation failed",
            ));
        }
    }
    Ok(root)
}

fn compare_required_subset(profile_name: &str, stable: &Path) -> Result<(), MachineError> {
    let manifest: Value = serde_json::from_str(MANIFEST).map_err(internal)?;
    let constraints = manifest["schema_constraints"].as_array().ok_or_else(|| {
        compatibility(
            profile_name,
            "required_subset",
            "required-subset manifest lacks schema_constraints",
        )
    })?;
    for constraint in constraints {
        let file = constraint["file"].as_str().ok_or_else(|| {
            compatibility(
                profile_name,
                "required_subset",
                "constraint file is invalid",
            )
        })?;
        let pointer = constraint["pointer"].as_str().ok_or_else(|| {
            compatibility(
                profile_name,
                "required_subset",
                "constraint pointer is invalid",
            )
        })?;
        let source = stable.join(file);
        let document = read_json(profile_name, &source)?;
        let value = resolve_pointer(profile_name, &source, &document, pointer)?;
        verify_constraint(profile_name, file, pointer, value.as_ref(), constraint)?;
    }
    Ok(())
}

fn verify_constraint(
    profile_name: &str,
    file: &str,
    pointer: &str,
    value: Option<&Value>,
    constraint: &Value,
) -> Result<(), MachineError> {
    let fail = || {
        compatibility(
            profile_name,
            "required_subset",
            format!("required schema constraint failed at {file}#{pointer}"),
        )
    };
    if constraint.get("absent") == Some(&Value::Bool(true)) {
        return if value.is_none() { Ok(()) } else { Err(fail()) };
    }
    let value = value.ok_or_else(fail)?;
    if let Some(expected) = constraint.get("equals")
        && value != expected
    {
        return Err(fail());
    }
    if let Some(required) = constraint.get("contains").and_then(Value::as_array) {
        let actual = value.as_array().ok_or_else(fail)?;
        if required.iter().any(|item| !actual.contains(item)) {
            return Err(fail());
        }
    }
    if let Some(required) = constraint
        .get("one_of_enum_contains")
        .and_then(Value::as_array)
    {
        let branches = value
            .get("oneOf")
            .and_then(Value::as_array)
            .or_else(|| value.as_array())
            .ok_or_else(fail)?;
        let actual = branches
            .iter()
            .filter_map(|branch| branch.get("enum"))
            .filter_map(Value::as_array)
            .flatten()
            .collect::<Vec<_>>();
        if required.iter().any(|item| !actual.contains(&item)) {
            return Err(fail());
        }
    }
    if let Some(spec) = constraint.get("one_of_property_enum_contains") {
        let property = spec["property"].as_str().ok_or_else(fail)?;
        let required = spec["values"].as_array().ok_or_else(fail)?;
        let branches = value
            .get("oneOf")
            .and_then(Value::as_array)
            .or_else(|| value.as_array())
            .ok_or_else(fail)?;
        let actual = branches
            .iter()
            .filter_map(|branch| branch.get("properties"))
            .filter_map(|properties| properties.get(property))
            .filter_map(|node| node.get("enum").or_else(|| node.get("const")))
            .flat_map(enum_or_scalar)
            .collect::<Vec<_>>();
        if required.iter().any(|item| !actual.contains(&item)) {
            return Err(fail());
        }
    }
    if let Some(spec) = constraint.get("one_of_title_required_contains") {
        let title = spec["title"].as_str().ok_or_else(fail)?;
        let required = spec["required"].as_array().ok_or_else(fail)?;
        let branches = value
            .get("oneOf")
            .and_then(Value::as_array)
            .or_else(|| value.as_array())
            .ok_or_else(fail)?;
        let branch = branches
            .iter()
            .find(|branch| branch["title"] == title)
            .ok_or_else(fail)?;
        let actual = branch["required"].as_array().ok_or_else(fail)?;
        if required.iter().any(|item| !actual.contains(item)) {
            return Err(fail());
        }
    }
    if let Some(spec) = constraint.get("one_of_title_property_contains") {
        let title = spec["title"].as_str().ok_or_else(fail)?;
        let required = spec["properties"].as_array().ok_or_else(fail)?;
        let branches = value
            .get("oneOf")
            .and_then(Value::as_array)
            .or_else(|| value.as_array())
            .ok_or_else(fail)?;
        let branch = branches
            .iter()
            .find(|branch| branch["title"] == title)
            .ok_or_else(fail)?;
        let actual = branch["properties"].as_object().ok_or_else(fail)?;
        if required
            .iter()
            .filter_map(Value::as_str)
            .any(|item| !actual.contains_key(item))
        {
            return Err(fail());
        }
    }
    Ok(())
}

fn enum_or_scalar(value: &Value) -> Vec<&Value> {
    value
        .as_array()
        .map_or_else(|| vec![value], |array| array.iter().collect())
}

fn resolve_pointer(
    profile_name: &str,
    source: &Path,
    document: &Value,
    pointer: &str,
) -> Result<Option<Value>, MachineError> {
    let Some(value) = document.pointer(pointer) else {
        return Ok(None);
    };
    resolve_refs(
        profile_name,
        source,
        document,
        value.clone(),
        &mut BTreeSet::new(),
    )
    .map(Some)
}

fn resolve_refs(
    profile_name: &str,
    source: &Path,
    document: &Value,
    value: Value,
    visited: &mut BTreeSet<String>,
) -> Result<Value, MachineError> {
    let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
        return Ok(value);
    };
    let identity = format!("{}#{reference}", source.display());
    if !visited.insert(identity) {
        return Err(compatibility(
            profile_name,
            "schema_reference",
            "schema contains a cyclic $ref",
        ));
    }
    let (target_file, fragment) = reference.split_once('#').unwrap_or((reference, ""));
    let (target_source, target_document) = if target_file.is_empty() {
        (source.to_path_buf(), document.clone())
    } else {
        let target = source
            .parent()
            .ok_or_else(|| {
                compatibility(
                    profile_name,
                    "schema_reference",
                    "schema source has no parent",
                )
            })?
            .join(target_file);
        let loaded = read_json(profile_name, &target)?;
        (target, loaded)
    };
    let pointer = if fragment.is_empty() { "" } else { fragment };
    let target = target_document.pointer(pointer).cloned().ok_or_else(|| {
        compatibility(
            profile_name,
            "schema_reference",
            format!("unresolved schema $ref {reference}"),
        )
    })?;
    resolve_refs(
        profile_name,
        &target_source,
        &target_document,
        target,
        visited,
    )
}

fn verify_exact_bundle_digests(
    profile_name: &str,
    root: &Path,
    version: &str,
    stable_digest: &str,
) -> Result<(), MachineError> {
    if version != SUPPORTED_CODEX_VERSION {
        return Ok(());
    }
    let manifest: Value = serde_json::from_str(MANIFEST).map_err(internal)?;
    let expected_stable = manifest["stable_schema_bundle_sha256"]
        .as_str()
        .ok_or_else(|| {
            compatibility(
                profile_name,
                "schema_bundle_digest",
                "manifest stable bundle digest is missing",
            )
        })?;
    let expected_experimental = manifest["experimental_schema_bundle_sha256"]
        .as_str()
        .ok_or_else(|| {
            compatibility(
                profile_name,
                "schema_bundle_digest",
                "manifest experimental bundle digest is missing",
            )
        })?;
    let actual_experimental = directory_sha256(profile_name, &root.join("experimental"))?;
    if stable_digest != expected_stable || actual_experimental != expected_experimental {
        return Err(compatibility(
            profile_name,
            "schema_bundle_digest",
            "exact-version schema bundle digest differs from the checked manifest",
        ));
    }
    Ok(())
}

fn normalized_argv(
    profile: &RuntimeProfile,
    executable: &Path,
) -> Result<Vec<String>, MachineError> {
    let mut argv = vec![path_utf8(executable)?.to_owned()];
    argv.extend(profile.argv.iter().skip(1).cloned());
    if !argv.iter().any(|argument| argument == "--strict-config") {
        argv.push("--strict-config".to_owned());
    }
    argv.extend(["--enable".to_owned(), "multi_agent".to_owned()]);
    Ok(argv)
}

/// Assembles the launch environment: the profile's own allowlisted map, the
/// canonical `CODEX_HOME`, and the reserved account and platform runtime
/// fields.
///
/// The five reserved names are *constructed*, never inherited. `getenv` would
/// report whatever the caller that started Dolgorae chose to export, so an
/// inherited `HOME` or `SHELL` would silently redirect the launched Codex's
/// login identity and an inherited `TMPDIR` would move its scratch state out
/// of the platform's per-uid directory — all of them caller-controlled inputs
/// entering a launch contract that is supposed to be a machine fact. They come
/// from the account database and the platform temporary-directory service
/// instead, and `parse_local_profiles` already refuses a profile that tries to
/// set any of them itself.
fn prepared_environment(
    profile: &RuntimeProfile,
) -> Result<BTreeMap<String, String>, MachineError> {
    validate_locale(&profile.environment["LANG"])?;
    validate_locale(&profile.environment["LC_ALL"])?;
    let mut environment = profile.environment.clone();
    environment.insert("CODEX_HOME".to_owned(), profile.codex_home.clone());
    let account = DarwinSystem.account_environment().map_err(|error| {
        MachineError::profile_config_invalid(
            "account",
            format!("account and platform runtime fields are unavailable: {error}"),
        )
    })?;
    let user = account.user.clone();
    for (name, value) in [
        ("HOME", reserved_path(&account.home, "HOME")?),
        ("USER", user.clone()),
        ("LOGNAME", user),
        ("SHELL", reserved_path(&account.shell, "SHELL")?),
        (
            "TMPDIR",
            reserved_path(&account.temporary_directory, "TMPDIR")?,
        ),
    ] {
        if value.is_empty() {
            return Err(MachineError::profile_config_invalid(
                name,
                "account or platform runtime field is empty",
            ));
        }
        environment.insert(name.to_owned(), value);
    }
    Ok(environment)
}

/// Renders one platform-supplied reserved path for the launch environment.
///
/// A relative or non-UTF-8 value is a broken account record, not something to
/// pass on to a launched process, so it fails the profile configuration
/// rather than travelling into the launch contract digest.
fn reserved_path(path: &Path, name: &str) -> Result<String, MachineError> {
    if !path.is_absolute() {
        return Err(MachineError::profile_config_invalid(
            name,
            format!("platform reported a non-absolute {name}"),
        ));
    }
    path.to_str().map(str::to_owned).ok_or_else(|| {
        MachineError::profile_config_invalid(name, format!("platform {name} is not UTF-8"))
    })
}

fn validate_locale(locale: &str) -> Result<(), MachineError> {
    let output = Command::new("/usr/bin/locale")
        .arg("-a")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .map_err(transport)?;
    if !output.status.success()
        || !output
            .stdout
            .split(|byte| *byte == b'\n')
            .any(|candidate| candidate == locale.as_bytes())
    {
        return Err(MachineError::profile_config_invalid(
            "locale",
            format!("locale {locale:?} is not available from the platform locale database"),
        ));
    }
    Ok(())
}

fn apply_environment(command: &mut Command, environment: &BTreeMap<String, String>) {
    command.env_clear();
    command.envs(environment);
}

fn start_snapshot(
    context: &Context,
    snapshot: &ProfileSnapshot,
    operator: &mut OperatorHandoff,
) -> Result<ServerState, MachineError> {
    start_snapshot_inner(
        context,
        snapshot,
        None,
        QuiescentRolloverPolicy::Allow,
        operator,
    )
}

fn start_snapshot_for_probe(
    context: &Context,
    snapshot: &ProfileSnapshot,
    operator: &mut OperatorHandoff,
) -> Result<ServerState, MachineError> {
    start_snapshot_inner(
        context,
        snapshot,
        None,
        QuiescentRolloverPolicy::Deny,
        operator,
    )
}

fn start_snapshot_for_migration(
    context: &Context,
    snapshot: &ProfileSnapshot,
    migration_id: Uuid,
    operator: &mut OperatorHandoff,
) -> Result<ServerState, MachineError> {
    start_snapshot_inner(
        context,
        snapshot,
        Some(migration_id),
        QuiescentRolloverPolicy::Deny,
        operator,
    )
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum QuiescentRolloverPolicy {
    Allow,
    Deny,
}

/// The Runtime Profile singleton start, split into the three phases ADR-016's
/// lock order requires.
///
/// PREPARE takes `home.lock` and `server.lock`, decides whether this call
/// attaches to a live server or reserves a new lifetime, and stamps the
/// reservation on the account home. APPLY runs with **no** lock held: it
/// spawns the drainer and the app-server, waits for the socket to bind, and
/// probes the server. COMMIT retakes both locks, revalidates the reservation
/// token, epoch, and process and socket identity, and only then publishes
/// `state.json` and the `ready` account-home record.
///
/// The split is not a refactor for its own sake. ADR-016 rejects waiting for
/// quiescence under `server.lock` because member paths need that same lock and
/// would deadlock against it, and a start's spawn, bind, and probe waits are
/// the same kind of wait: bounded by `SOCKET_BIND_BUDGET` plus a 30-second
/// probe connect, all of it previously spent holding both locks and the
/// `CODEX_HOME` they serialize.
fn start_snapshot_inner(
    context: &Context,
    snapshot: &ProfileSnapshot,
    migration_id: Option<Uuid>,
    rollover_policy: QuiescentRolloverPolicy,
    operator: &mut OperatorHandoff,
) -> Result<ServerState, MachineError> {
    if snapshot.compatibility_verdict == CompatibilityVerdict::Rejected {
        return Err(compatibility(
            &snapshot.profile_name,
            "compatibility_verdict",
            "rejected profile cannot start",
        ));
    }
    let paths = LifecyclePaths::open(context, snapshot)?;
    let reservation = match prepare_start(&paths, snapshot, migration_id, operator)? {
        StartPrepared::Attached(state) => return Ok(*state),
        StartPrepared::QuiescentRollover(old_server_key) => {
            if rollover_policy == QuiescentRolloverPolicy::Deny {
                return Err(profile_launch_conflict(
                    &snapshot.profile_name,
                    &old_server_key,
                    "this start policy does not permit replacing a different active launch contract",
                ));
            }
            return automatic_quiescent_migration(context, snapshot, &old_server_key);
        }
        StartPrepared::Reserved(reservation) => reservation,
    };
    let applied = match apply_start(&paths, snapshot, &reservation) {
        Ok(applied) => applied,
        Err(error) => {
            release_reservation(&paths, snapshot, &reservation);
            return Err(error);
        }
    };
    match commit_start(&paths, context, snapshot, &reservation, &applied) {
        Ok(state) => Ok(state),
        Err(error) => {
            cleanup_failed_start(
                &reservation.socket,
                &applied.process_identity,
                &applied.drainer_identity,
            );
            release_reservation(&paths, snapshot, &reservation);
            Err(error)
        }
    }
}

/// The directories and files one profile lifecycle operation works in.
struct LifecyclePaths {
    root: PathBuf,
    home_root: PathBuf,
    state_path: PathBuf,
    active_path: PathBuf,
}

impl LifecyclePaths {
    fn open(context: &Context, snapshot: &ProfileSnapshot) -> Result<Self, MachineError> {
        let root = profile_root(context, snapshot);
        secure_dir(&root)?;
        let home_root = home_root(context, &snapshot.canonical_codex_home)?;
        secure_dir(&home_root)?;
        Ok(Self {
            state_path: root.join("state.json"),
            active_path: home_root.join("active.json"),
            root,
            home_root,
        })
    }

    /// Takes `home.lock` and then `server.lock`, in that normative order.
    fn lock(&self) -> Result<(File, File), MachineError> {
        let home_lock = lock_file(&self.home_root.join("home.lock"))?;
        let server_lock = lock_file(&self.root.join("server.lock"))?;
        Ok((home_lock, server_lock))
    }

    fn membership_path(&self) -> PathBuf {
        self.root.join("membership.jsonl")
    }
}

/// What PREPARE decided.
enum StartPrepared {
    /// A verified live server already owns this contract; nothing to spawn.
    Attached(Box<ServerState>),
    /// Another contract owns this account home. The caller must leave
    /// PREPARE's locks before proving that lifetime quiescent and replacing it.
    QuiescentRollover(String),
    /// This call owns the next lifetime and holds the reservation for it.
    Reserved(StartReservation),
}

/// The lifetime PREPARE reserved for APPLY to fill and COMMIT to publish.
struct StartReservation {
    epoch: u64,
    token: Uuid,
    socket: PathBuf,
}

/// What APPLY produced, all of it re-proved by COMMIT before publication.
struct StartApplied {
    pid: u32,
    process_identity: crate::darwin::LiveProcessIdentity,
    drainer_pid: u32,
    drainer_identity: crate::darwin::LiveProcessIdentity,
    socket_device: u64,
    socket_inode: u64,
    probe: ProbeResult,
}

/// PREPARE: under `home.lock` and `server.lock`, either attach to the live
/// server or reserve the next lifetime. Never waits on a process.
fn prepare_start(
    paths: &LifecyclePaths,
    snapshot: &ProfileSnapshot,
    migration_id: Option<Uuid>,
    operator: &mut OperatorHandoff,
) -> Result<StartPrepared, MachineError> {
    let (home_lock, server_lock) = paths.lock()?;
    // The operator-authorized prefix is now complete, so the hold that
    // authorized this start can retire into the locks that replace it.
    operator.handoff(&home_lock);
    verify_migration_fence(
        &paths.home_root.join("migration.json"),
        migration_id,
        &snapshot.profile_name,
        &snapshot.server_key,
    )?;
    let active = read_home_active(&paths.active_path)?;
    if let Some(state) = read_state_if_running(&paths.state_path)? {
        let attached = attach_running(snapshot, &state, active.as_ref())?;
        drop(server_lock);
        drop(home_lock);
        return Ok(StartPrepared::Attached(Box::new(attached)));
    }
    if let Some(active) = active.as_ref() {
        if active.server_key != snapshot.server_key {
            if migration_id.is_some() {
                return Err(profile_launch_conflict(
                    &snapshot.profile_name,
                    &active.server_key,
                    format!(
                        "another launch contract is active for this CODEX_HOME; requested {}",
                        snapshot.server_key
                    ),
                ));
            }
            let old_server_key = active.server_key.clone();
            drop(server_lock);
            drop(home_lock);
            return Ok(StartPrepared::QuiescentRollover(old_server_key));
        }
        // A `starting` or `stopping` record is another call's reservation in
        // its lock-free APPLY window. Spawning a second app-server onto the
        // same socket would be the exact double-launch the singleton exists to
        // prevent, so this waits for the other transition instead. A record a
        // crash stranded is cleared by `profile state reset`.
        if active.lifecycle != "ready" {
            return Err(profile_server_busy(
                &snapshot.profile_name,
                &snapshot.server_key,
                format!(
                    "another lifecycle transition holds this CODEX_HOME in {}",
                    active.lifecycle
                ),
            ));
        }
    }
    let socket = socket_path(&snapshot.server_key)?;
    clear_stale_socket(snapshot, &socket, &paths.state_path)?;
    let epoch = reserve_epoch(&paths.root.join("epoch"))?;
    let token = Uuid::now_v7();
    write_home_active(
        &paths.active_path,
        &HomeActive {
            schema_version: 1,
            canonical_codex_home: snapshot.canonical_codex_home.clone(),
            server_key: snapshot.server_key.clone(),
            server_epoch: epoch,
            lifecycle: "starting".to_owned(),
            pid: None,
            transition_token: Some(token),
        },
    )?;
    drop(server_lock);
    drop(home_lock);
    Ok(StartPrepared::Reserved(StartReservation {
        epoch,
        token,
        socket,
    }))
}

/// Verifies that a recorded running server is still the one this contract
/// describes before a caller attaches to it.
///
/// Attaching is how every Run reaches the singleton, so it is the same
/// identity question a stop asks and it gets the same answer: the account-home
/// record must agree with the recorded state, and the socket the caller is
/// about to be handed must still be the exact device and inode the start
/// recorded. A path check alone proves nothing — the socket file can be
/// replaced by an unrelated one at the same pathname between the two
/// lifetimes, and a caller handed that path would connect to whatever now
/// answers there.
fn attach_running(
    snapshot: &ProfileSnapshot,
    state: &ServerState,
    active: Option<&HomeActive>,
) -> Result<ServerState, MachineError> {
    let expected_active = json!({
        "server_key": state.server_key,
        "server_epoch": state.server_epoch,
        "pid": state.pid,
        "lifecycle": "ready",
    });
    let active = active.ok_or_else(|| {
        profile_mismatch(
            &snapshot.profile_name,
            "home_active_contract",
            expected_active.clone(),
            Value::Null,
        )
    })?;
    if active.server_key == state.server_key
        && active.server_epoch == state.server_epoch
        && active.lifecycle != "ready"
    {
        return Err(profile_server_busy(
            &snapshot.profile_name,
            &snapshot.server_key,
            format!(
                "another lifecycle transition holds this CODEX_HOME in {}",
                active.lifecycle
            ),
        ));
    }
    if active.server_key != state.server_key
        || active.server_epoch != state.server_epoch
        || active.pid != Some(state.pid)
        || active.lifecycle != "ready"
    {
        return Err(profile_mismatch(
            &snapshot.profile_name,
            "home_active_contract",
            expected_active,
            json!({
                "server_key": active.server_key,
                "server_epoch": active.server_epoch,
                "pid": active.pid,
                "lifecycle": active.lifecycle,
            }),
        ));
    }
    verify_recorded_socket(&snapshot.profile_name, state)?;
    Ok(state.clone())
}

/// Proves the socket a recorded server state names is still that exact
/// socket: same device, same inode, still a socket.
fn verify_recorded_socket(profile_name: &str, state: &ServerState) -> Result<(), MachineError> {
    let path = Path::new(&state.socket_path);
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        profile_mismatch(
            profile_name,
            "socket_identity",
            json!({
                "socket_path": state.socket_path,
                "socket_device": state.socket_device,
                "socket_inode": state.socket_inode,
                "is_socket": true,
            }),
            json!({"socket_path": state.socket_path, "error": error.to_string()}),
        )
    })?;
    if metadata.dev() != state.socket_device
        || metadata.ino() != state.socket_inode
        || !metadata.file_type().is_socket()
    {
        return Err(profile_mismatch(
            profile_name,
            "socket_identity",
            json!({
                "socket_device": state.socket_device,
                "socket_inode": state.socket_inode,
                "is_socket": true,
            }),
            json!({
                "socket_device": metadata.dev(),
                "socket_inode": metadata.ino(),
                "is_socket": metadata.file_type().is_socket(),
            }),
        ));
    }
    Ok(())
}

/// Removes a socket file left behind by a lifetime this profile can prove is
/// over, and refuses to touch one it cannot.
fn clear_stale_socket(
    snapshot: &ProfileSnapshot,
    socket: &Path,
    state_path: &Path,
) -> Result<(), MachineError> {
    let Ok(metadata) = fs::symlink_metadata(socket) else {
        return Ok(());
    };
    if !metadata.file_type().is_socket() {
        return Err(profile_mismatch(
            &snapshot.profile_name,
            "socket_identity",
            json!("socket"),
            json!(format!("{:?}", metadata.file_type())),
        ));
    }
    let stale = read_state(state_path)?.ok_or_else(|| {
        profile_mismatch(
            &snapshot.profile_name,
            "socket_stale_record",
            json!(true),
            json!(false),
        )
    })?;
    let stale_still_alive =
        process_identity_matches(stale.pid, stale.uid, stale.pgid, &stale.process_fingerprint);
    if stale.server_key != snapshot.server_key
        || stale.socket_path != path_utf8(socket)?
        || stale.socket_device != metadata.dev()
        || stale.socket_inode != metadata.ino()
        || stale_still_alive
    {
        return Err(profile_mismatch(
            &snapshot.profile_name,
            "socket_stale_identity",
            json!({
                "server_key": snapshot.server_key,
                "socket_path": path_utf8(socket)?,
                "socket_device": metadata.dev(),
                "socket_inode": metadata.ino(),
                "pid_alive": false,
            }),
            json!({
                "server_key": stale.server_key,
                "socket_path": stale.socket_path,
                "socket_device": stale.socket_device,
                "socket_inode": stale.socket_inode,
                "pid_alive": stale_still_alive,
            }),
        ));
    }
    fs::remove_file(socket).map_err(io_error)
}

/// APPLY: spawn the drainer and the app-server, wait for the socket, and probe
/// it. Holds no home or server lock; the account-home record is only touched
/// through `amend_reservation`, which takes `home.lock` for one atomic write
/// and never waits under it.
fn apply_start(
    paths: &LifecyclePaths,
    snapshot: &ProfileSnapshot,
    reservation: &StartReservation,
) -> Result<StartApplied, MachineError> {
    let (stdout_read, stdout_write) = UnixStream::pair().map_err(io_error)?;
    let (stderr_read, stderr_write) = UnixStream::pair().map_err(io_error)?;
    let mut drainer_command = Command::new(std::env::current_exe().map_err(io_error)?);
    drainer_command
        .arg(PROFILE_LOG_DRAINER_COMMAND)
        .arg("--root")
        .arg(&paths.root)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut drainer = DarwinSystem
        .spawn_detached_with_log_fds(
            &mut drainer_command,
            stdout_read.as_raw_fd(),
            stderr_read.as_raw_fd(),
        )
        .map_err(transport)?;
    let drainer_pid = drainer.id();
    drop(stdout_read);
    drop(stderr_read);
    let drainer_identity = match wait_for_drainer_identity(drainer_pid) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = drainer.kill();
            let _ = drainer.wait();
            return Err(transport(error));
        }
    };
    std::mem::forget(drainer);
    let mut command = Command::new(&snapshot.normalized_argv[0]);
    command.args(&snapshot.normalized_argv[1..]);
    command.args(["app-server", "--listen"]);
    command.arg(format!("unix://{}", path_utf8(&reservation.socket)?));
    command.current_dir(&snapshot.derived_launch_cwd);
    apply_environment(&mut command, &snapshot.sanitized_environment);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::os::fd::OwnedFd::from(stdout_write)))
        .stderr(Stdio::from(std::os::fd::OwnedFd::from(stderr_write)));
    let mut child = match DarwinSystem.spawn_detached(&mut command) {
        Ok(child) => child,
        Err(error) => {
            cleanup_verified_process(&drainer_identity);
            return Err(transport(error));
        }
    };
    let pid = child.id();
    let mut process_identity = match wait_for_app_identity(pid) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            cleanup_verified_process(&drainer_identity);
            return Err(transport(error));
        }
    };
    std::mem::forget(child);
    // `verified` always carries the newest proven identity, so the cleanup
    // below signals the process this start actually last observed rather than
    // a sample the app-server may have moved on from.
    let applied = (|verified: &mut crate::darwin::LiveProcessIdentity| -> Result<StartApplied, MachineError> {
        // Record the PID before the bind wait so a crash inside APPLY leaves
        // the account home naming the process an operator has to reap, rather
        // than an unattributable orphan.
        amend_reservation(paths, snapshot, reservation, |active| {
            active.pid = Some(pid);
        })?;
        wait_for_socket(&reservation.socket, pid)?;
        *verified = wait_for_app_identity(pid).map_err(transport)?;
        if verified.uid != DarwinSystem.current_uid() || verified.process_group_id != pid {
            return Err(transport("app-server process identity is invalid"));
        }
        let metadata = fs::symlink_metadata(&reservation.socket).map_err(io_error)?;
        if !metadata.file_type().is_socket() {
            return Err(transport("app-server did not bind a Unix socket"));
        }
        fs::set_permissions(&reservation.socket, fs::Permissions::from_mode(0o600))
            .map_err(io_error)?;
        let probe = probe_server(
            &snapshot.profile_name,
            &reservation.socket,
            &snapshot.canonical_codex_home,
        )?;
        Ok(StartApplied {
            pid,
            process_identity: verified.clone(),
            drainer_pid,
            drainer_identity: drainer_identity.clone(),
            socket_device: metadata.dev(),
            socket_inode: metadata.ino(),
            probe,
        })
    })(&mut process_identity);
    applied.inspect_err(|_| {
        cleanup_failed_start(&reservation.socket, &process_identity, &drainer_identity);
    })
}

/// COMMIT: retake both locks, prove the reservation still holds, and publish.
fn commit_start(
    paths: &LifecyclePaths,
    context: &Context,
    snapshot: &ProfileSnapshot,
    reservation: &StartReservation,
    applied: &StartApplied,
) -> Result<ServerState, MachineError> {
    let (home_lock, server_lock) = paths.lock()?;
    require_reservation(paths, snapshot, reservation, "starting")?;
    // A record left by a lifetime that is provably over is what this start is
    // replacing; only a *live* one means another start won the race.
    if read_state_if_running(&paths.state_path)?.is_some() {
        return Err(profile_server_busy(
            &snapshot.profile_name,
            &snapshot.server_key,
            "another start published a live server state while this one was spawning",
        ));
    }
    // Identity: the process this start is about to publish must still be the
    // one it verified, and the socket must still be the one it measured.
    let live = DarwinSystem
        .live_process_identity(applied.pid)
        .map_err(transport)?;
    if live != applied.process_identity {
        return Err(profile_mismatch(
            &snapshot.profile_name,
            "process_identity",
            json!({
                "uid": applied.process_identity.uid,
                "process_group_id": applied.process_identity.process_group_id,
                "fingerprint": applied.process_identity.fingerprint,
            }),
            json!({
                "uid": live.uid,
                "process_group_id": live.process_group_id,
                "fingerprint": live.fingerprint,
            }),
        ));
    }
    let metadata = fs::symlink_metadata(&reservation.socket).map_err(io_error)?;
    if !metadata.file_type().is_socket()
        || metadata.dev() != applied.socket_device
        || metadata.ino() != applied.socket_inode
    {
        return Err(profile_mismatch(
            &snapshot.profile_name,
            "socket_identity",
            json!({
                "socket_device": applied.socket_device,
                "socket_inode": applied.socket_inode,
                "is_socket": true,
            }),
            json!({
                "socket_device": metadata.dev(),
                "socket_inode": metadata.ino(),
                "is_socket": metadata.file_type().is_socket(),
            }),
        ));
    }
    let scope = MembershipScope {
        profile: snapshot.profile_name.clone(),
        server_key: snapshot.server_key.clone(),
    };
    let membership_path = paths.membership_path();
    append_membership(
        &membership_path,
        "server_started",
        &context.workspace_id,
        None,
        &scope,
    )?;
    let membership_revision = replay_membership(&membership_path, &scope)?
        .last()
        .map_or(0, |record| record.revision);
    let state = ServerState {
        schema_version: 1,
        server_key: snapshot.server_key.clone(),
        lifecycle: "ready".to_owned(),
        server_epoch: reservation.epoch,
        epoch_id: Uuid::now_v7(),
        pid: applied.pid,
        pgid: applied.pid,
        uid: applied.process_identity.uid,
        process_fingerprint: applied.process_identity.fingerprint.clone(),
        drainer_pid: applied.drainer_pid,
        drainer_pgid: applied.drainer_pid,
        drainer_uid: applied.drainer_identity.uid,
        drainer_fingerprint: applied.drainer_identity.fingerprint.clone(),
        socket_path: path_utf8(&reservation.socket)?.to_owned(),
        socket_device: applied.socket_device,
        socket_inode: applied.socket_inode,
        membership_revision,
        default_model: applied.probe.default_model.clone(),
        models: applied.probe.models.clone(),
        capabilities: applied.probe.capabilities.clone(),
        snapshot: snapshot.clone(),
    };
    atomic_replace(
        &paths.state_path,
        &serde_json::to_vec_pretty(&state).map_err(internal)?,
    )?;
    write_home_active(
        &paths.active_path,
        &HomeActive {
            schema_version: 1,
            canonical_codex_home: snapshot.canonical_codex_home.clone(),
            server_key: snapshot.server_key.clone(),
            server_epoch: reservation.epoch,
            lifecycle: "ready".to_owned(),
            pid: Some(applied.pid),
            transition_token: None,
        },
    )?;
    append_diagnostic(
        &paths.root,
        "server_ready",
        json!({"pid": applied.pid, "epoch": reservation.epoch}),
    )?;
    drop(server_lock);
    drop(home_lock);
    Ok(state)
}

/// Proves the account home still carries the reservation PREPARE stamped:
/// same server key, same epoch, same token, same phase.
///
/// This is the check that makes releasing the locks for APPLY safe. Anything
/// that could have replaced the reservation — a competing start, an operator
/// state reset, a migration — necessarily rewrote or removed this record, so a
/// commit that still finds its own token is committing into the same lifetime
/// it reserved.
fn require_reservation(
    paths: &LifecyclePaths,
    snapshot: &ProfileSnapshot,
    reservation: &StartReservation,
    phase: &str,
) -> Result<(), MachineError> {
    require_transition(
        &paths.active_path,
        snapshot,
        reservation.epoch,
        reservation.token,
        phase,
    )
}

fn require_transition(
    active_path: &Path,
    snapshot: &ProfileSnapshot,
    epoch: u64,
    token: Uuid,
    phase: &str,
) -> Result<(), MachineError> {
    let expected = json!({
        "server_key": snapshot.server_key,
        "server_epoch": epoch,
        "lifecycle": phase,
        "transition_token": token,
    });
    let active = read_home_active(active_path)?.ok_or_else(|| {
        profile_mismatch(
            &snapshot.profile_name,
            "home_transition_token",
            expected.clone(),
            Value::Null,
        )
    })?;
    if active.server_key != snapshot.server_key
        || active.server_epoch != epoch
        || active.lifecycle != phase
        || active.transition_token != Some(token)
    {
        return Err(profile_mismatch(
            &snapshot.profile_name,
            "home_transition_token",
            expected,
            json!({
                "server_key": active.server_key,
                "server_epoch": active.server_epoch,
                "lifecycle": active.lifecycle,
                "transition_token": active.transition_token,
            }),
        ));
    }
    Ok(())
}

/// Rewrites the reserved account-home record under `home.lock`, revalidating
/// the reservation first. One atomic write, no waiting under the lock.
fn amend_reservation(
    paths: &LifecyclePaths,
    snapshot: &ProfileSnapshot,
    reservation: &StartReservation,
    amend: impl FnOnce(&mut HomeActive),
) -> Result<(), MachineError> {
    let home_lock = lock_file(&paths.home_root.join("home.lock"))?;
    require_reservation(paths, snapshot, reservation, "starting")?;
    let mut active = read_home_active(&paths.active_path)?
        .ok_or_else(|| transport("home active contract disappeared under its own lock"))?;
    amend(&mut active);
    let result = write_home_active(&paths.active_path, &active);
    drop(home_lock);
    result
}

/// Gives up a reservation this start could not fulfil.
///
/// Best effort: it retakes `home.lock`, and only removes the record when the
/// token proves the record is still this start's own. A record some other
/// transition has since claimed is left exactly as that transition wrote it.
fn release_reservation(
    paths: &LifecyclePaths,
    snapshot: &ProfileSnapshot,
    reservation: &StartReservation,
) {
    let Ok(home_lock) = lock_file(&paths.home_root.join("home.lock")) else {
        return;
    };
    if read_home_active(&paths.active_path).is_ok_and(|active| {
        active.is_some_and(|active| {
            active.server_key == snapshot.server_key
                && active.server_epoch == reservation.epoch
                && active.transition_token == Some(reservation.token)
        })
    }) {
        let _ = fs::remove_file(&paths.active_path);
        let _ = sync_parent(&paths.active_path);
    }
    drop(home_lock);
}

fn wait_for_drainer_identity(
    pid: u32,
) -> Result<crate::darwin::LiveProcessIdentity, std::io::Error> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut previous = None;
    loop {
        if let Ok(identity) = DarwinSystem.live_process_identity(pid)
            && identity.process_group_id == pid
            && identity.fingerprint.contains("__profile-log-drainer")
        {
            if previous.as_ref() == Some(&identity) {
                return Ok(identity);
            }
            previous = Some(identity);
        } else {
            previous = None;
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "profile log drainer did not establish a stable identity",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_app_identity(pid: u32) -> Result<crate::darwin::LiveProcessIdentity, std::io::Error> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut previous = None;
    loop {
        if let Ok(identity) = DarwinSystem.live_process_identity(pid)
            && identity.process_group_id == pid
            && identity.fingerprint.contains("app-server")
        {
            if previous.as_ref() == Some(&identity) {
                return Ok(identity);
            }
            previous = Some(identity);
        } else {
            previous = None;
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "app-server did not establish a stable identity",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn verify_migration_fence(
    path: &Path,
    authorized: Option<Uuid>,
    profile: &str,
    server_key: &str,
) -> Result<(), MachineError> {
    if !path.exists() {
        return Ok(());
    }
    let (value, parsed) = read_migration_record(path)?;
    if parsed.phase.is_terminal() {
        return Ok(());
    }
    if authorized == Some(parsed.migration_id) {
        return Ok(());
    }
    Err(profile_server_busy(
        profile,
        server_key,
        format!(
            "the CODEX_HOME is fenced by an active migration ({})",
            value["migration_id"].as_str().unwrap_or("unknown")
        ),
    ))
}

/// The Runtime Profile singleton stop, split into the same three phases as the
/// start.
///
/// PREPARE takes `home.lock` and `server.lock`, proves the recorded server is
/// the one this contract names, gates on live members, and stamps a stopping
/// reservation. APPLY signals the process group and waits for it to exit with
/// **no** lock held — ADR-016 rejects waiting for quiescence under
/// `server.lock` precisely because a member's own shutdown path needs that
/// lock and the two would deadlock. COMMIT retakes both locks, revalidates the
/// token, epoch, and socket identity, and removes the state and the
/// account-home record.
fn stop_snapshot(
    context: &Context,
    snapshot: &ProfileSnapshot,
    interrupt: bool,
    operator: &mut OperatorHandoff,
) -> Result<bool, MachineError> {
    stop_snapshot_inner(context, snapshot, None, interrupt, operator)
}

fn stop_snapshot_for_migration(
    context: &Context,
    snapshot: &ProfileSnapshot,
    migration_id: Uuid,
    interrupt: bool,
    operator: &mut OperatorHandoff,
) -> Result<bool, Box<MigrationStopFailure>> {
    let paths = LifecyclePaths::open(context, snapshot)
        .map_err(MigrationStopFailure::before_stop)
        .map_err(Box::new)?;
    let Some(reservation) = prepare_stop(&paths, snapshot, Some(migration_id), interrupt, operator)
        .map_err(MigrationStopFailure::before_stop)
        .map_err(Box::new)?
    else {
        return Ok(false);
    };
    if let Err(error) = apply_stop(&reservation.state) {
        let process_stopped = !process_exists(reservation.state.pid);
        if !process_stopped {
            restore_stopping_reservation(&paths, snapshot, &reservation);
        }
        return Err(Box::new(MigrationStopFailure {
            error,
            reservation: Some(reservation),
            process_stopped,
        }));
    }
    commit_stop(&paths, snapshot, &reservation).map_err(|error| {
        Box::new(MigrationStopFailure {
            error,
            reservation: Some(reservation),
            process_stopped: true,
        })
    })?;
    Ok(true)
}

struct MigrationStopFailure {
    error: MachineError,
    reservation: Option<StopReservation>,
    process_stopped: bool,
}

impl MigrationStopFailure {
    fn before_stop(error: MachineError) -> Self {
        Self {
            error,
            reservation: None,
            process_stopped: false,
        }
    }
}

fn stop_snapshot_inner(
    context: &Context,
    snapshot: &ProfileSnapshot,
    migration_id: Option<Uuid>,
    interrupt: bool,
    operator: &mut OperatorHandoff,
) -> Result<bool, MachineError> {
    let paths = LifecyclePaths::open(context, snapshot)?;
    let Some(reservation) = prepare_stop(&paths, snapshot, migration_id, interrupt, operator)?
    else {
        return Ok(false);
    };
    if let Err(error) = apply_stop(&reservation.state) {
        restore_stopping_reservation(&paths, snapshot, &reservation);
        return Err(error);
    }
    commit_stop(&paths, snapshot, &reservation).map(|()| true)
}

/// The stopping lifetime PREPARE reserved.
struct StopReservation {
    state: ServerState,
    token: Uuid,
}

/// PREPARE: prove the recorded server, gate on live members, and mark the
/// account home `stopping`. Never signals and never waits on a process.
fn prepare_stop(
    paths: &LifecyclePaths,
    snapshot: &ProfileSnapshot,
    migration_id: Option<Uuid>,
    interrupt: bool,
    operator: &mut OperatorHandoff,
) -> Result<Option<StopReservation>, MachineError> {
    let (home_lock, server_lock) = paths.lock()?;
    // The operator-authorized prefix is now complete, so the hold that
    // authorized this stop can retire into the locks that replace it. Waiting
    // for the server to quiesce happens in APPLY, under no lock at all.
    operator.handoff(&home_lock);
    verify_migration_fence(
        &paths.home_root.join("migration.json"),
        migration_id,
        &snapshot.profile_name,
        &snapshot.server_key,
    )?;
    let Some(state) = read_state_if_running(&paths.state_path)? else {
        return Ok(None);
    };
    if state.server_key != snapshot.server_key
        || state.pid != state.pgid
        || state.uid != DarwinSystem.current_uid()
        || state.drainer_pid != state.drainer_pgid
        || state.drainer_uid != DarwinSystem.current_uid()
    {
        let current_uid = DarwinSystem.current_uid();
        return Err(profile_mismatch(
            &snapshot.profile_name,
            "process_identity",
            json!({
                "server_key": snapshot.server_key,
                "pid_eq_pgid": true,
                "uid": current_uid,
                "drainer_pid_eq_pgid": true,
                "drainer_uid": current_uid,
            }),
            json!({
                "server_key": state.server_key,
                "pid_eq_pgid": state.pid == state.pgid,
                "uid": state.uid,
                "drainer_pid_eq_pgid": state.drainer_pid == state.drainer_pgid,
                "drainer_uid": state.drainer_uid,
            }),
        ));
    }
    verify_live_process_identity(&state)?;
    let active = read_home_active(&paths.active_path)?;
    verify_stop_home_active(snapshot, &state, active.as_ref())?;
    let scope = MembershipScope {
        profile: snapshot.profile_name.clone(),
        server_key: snapshot.server_key.clone(),
    };
    let membership = replay_membership(&paths.membership_path(), &scope)?;
    if membership.last().map_or(0, |record| record.revision) != state.membership_revision {
        return Err(scope.incomplete("membership journal and server state revisions differ"));
    }
    let index = derive_membership_index(&paths.root, &membership)?;
    let members = live_member_evidence(&index);
    if !members.is_empty() && !interrupt {
        return Err(scope.incomplete(format!(
            "stopping this server would interrupt {} live run member(s); \
             pass --interrupt with --confirm-server-key to authorize it",
            members.len()
        )));
    }
    if interrupt {
        // The interrupt is recorded with the members it actually interrupted,
        // not as a bare flag: an operator reading the journal afterwards has
        // to be able to see which Runs the confirmed server key cost. Those
        // are Run identities, so the record is operator-projected and the
        // minimal projection sees only that an interrupt happened.
        append_diagnostic_record(
            &paths.root,
            "operator_interrupt",
            json!({
                "pid": state.pid,
                "confirmed_server_key": snapshot.server_key,
                "interrupted_members": members,
            }),
            DiagnosticProjection::Operational,
        )?;
    }
    let token = Uuid::now_v7();
    write_home_active(
        &paths.active_path,
        &HomeActive {
            schema_version: 1,
            canonical_codex_home: snapshot.canonical_codex_home.clone(),
            server_key: snapshot.server_key.clone(),
            server_epoch: state.server_epoch,
            lifecycle: "stopping".to_owned(),
            pid: Some(state.pid),
            transition_token: Some(token),
        },
    )?;
    drop(server_lock);
    drop(home_lock);
    Ok(Some(StopReservation { state, token }))
}

fn verify_stop_home_active(
    snapshot: &ProfileSnapshot,
    state: &ServerState,
    active: Option<&HomeActive>,
) -> Result<(), MachineError> {
    let active = active.ok_or_else(|| {
        profile_mismatch(
            &snapshot.profile_name,
            "home_active_contract",
            json!({
                "server_key": snapshot.server_key,
                "server_epoch": state.server_epoch,
                "lifecycle": "ready",
                "pid": state.pid,
            }),
            Value::Null,
        )
    })?;
    if active.server_key != snapshot.server_key
        || active.server_epoch != state.server_epoch
        || active.lifecycle != "ready"
        || active.pid != Some(state.pid)
        || active.transition_token.is_some()
    {
        return Err(profile_server_busy(
            &snapshot.profile_name,
            &active.server_key,
            format!(
                "another lifecycle transition holds this CODEX_HOME in {}",
                active.lifecycle
            ),
        ));
    }
    Ok(())
}

/// The live members a stop would interrupt, as the evidence the interrupt
/// diagnostic and the gate's message both name.
fn live_member_evidence(index: &MembershipIndex) -> Vec<Value> {
    run_members(index)
        .into_iter()
        .map(|member| json!({"workspace_id": member.workspace_id, "run_id": member.run_id}))
        .collect()
}

/// APPLY: signal the process group and wait for it to go. No lock held.
fn apply_stop(state: &ServerState) -> Result<(), MachineError> {
    DarwinSystem
        .signal_process_group(state.pgid, libc::SIGTERM)
        .map_err(transport)?;
    if !wait_for_exit(state.pid, TERMINATE_BUDGET) {
        verify_live_process_identity(state)?;
        DarwinSystem
            .signal_process_group(state.pgid, libc::SIGKILL)
            .map_err(transport)?;
        let _ = wait_for_exit(state.pid, KILL_BUDGET);
    }
    let _ = DarwinSystem.reap_child_nonblocking(state.pid);
    if process_exists(state.pid) {
        return Err(transport("profile process group did not terminate"));
    }
    stop_drainer(state)
}

fn wait_for_exit(pid: u32, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while process_exists(pid) && Instant::now() < deadline {
        if DarwinSystem.reap_child_nonblocking(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !process_exists(pid)
}

/// COMMIT: retake both locks, prove the stopping reservation still holds and
/// the socket is still the recorded one, then remove the state.
fn commit_stop(
    paths: &LifecyclePaths,
    snapshot: &ProfileSnapshot,
    reservation: &StopReservation,
) -> Result<(), MachineError> {
    let (home_lock, server_lock) = paths.lock()?;
    require_transition(
        &paths.active_path,
        snapshot,
        reservation.state.server_epoch,
        reservation.token,
        "stopping",
    )?;
    // Identity: the state file being removed must still be the exact epoch
    // this stop signalled, never a fresh lifetime some other call published
    // while APPLY was waiting.
    match read_state(&paths.state_path)? {
        Some(current) if current.epoch_id == reservation.state.epoch_id => {}
        Some(current) => {
            return Err(profile_mismatch(
                &snapshot.profile_name,
                "server_epoch_identity",
                json!({
                    "server_epoch": reservation.state.server_epoch,
                    "epoch_id": reservation.state.epoch_id,
                }),
                json!({"server_epoch": current.server_epoch, "epoch_id": current.epoch_id}),
            ));
        }
        None => {
            return Err(profile_mismatch(
                &snapshot.profile_name,
                "server_epoch_identity",
                json!({
                    "server_epoch": reservation.state.server_epoch,
                    "epoch_id": reservation.state.epoch_id,
                }),
                Value::Null,
            ));
        }
    }
    if fs::symlink_metadata(&reservation.state.socket_path).is_ok() {
        verify_recorded_socket(&snapshot.profile_name, &reservation.state)?;
        fs::remove_file(&reservation.state.socket_path).map_err(io_error)?;
    }
    fs::remove_file(&paths.state_path).map_err(io_error)?;
    sync_parent(&paths.state_path)?;
    clear_home_active(
        &snapshot.profile_name,
        &paths.active_path,
        &snapshot.server_key,
    )?;
    append_diagnostic(
        &paths.root,
        "server_stopped",
        json!({
            "pid": reservation.state.pid,
            "epoch": reservation.state.server_epoch,
        }),
    )?;
    drop(server_lock);
    drop(home_lock);
    Ok(())
}

/// Completes a migration-owned stop whose process termination succeeded but
/// whose durable COMMIT returned an error. This cleanup is idempotent so a
/// partial first COMMIT (socket or state already removed) can still reach the
/// stopped boundary required before restoring the old snapshot.
fn settle_terminated_migration_stop(
    context: &Context,
    snapshot: &ProfileSnapshot,
    reservation: &StopReservation,
) -> Result<(), MachineError> {
    if process_exists(reservation.state.pid) {
        return Err(transport(
            "migration stop settlement found the old process still running",
        ));
    }
    stop_drainer(&reservation.state)?;
    let paths = LifecyclePaths::open(context, snapshot)?;
    let (home_lock, server_lock) = paths.lock()?;
    if let Some(active) = read_home_active(&paths.active_path)?
        && (active.server_key != snapshot.server_key
            || active.server_epoch != reservation.state.server_epoch
            || active.lifecycle != "stopping"
            || active.transition_token != Some(reservation.token))
    {
        return Err(profile_server_busy(
            &snapshot.profile_name,
            &active.server_key,
            format!(
                "another lifecycle transition replaced the migration stop in {}",
                active.lifecycle
            ),
        ));
    }
    if let Some(current) = read_state(&paths.state_path)? {
        if current.epoch_id != reservation.state.epoch_id {
            return Err(profile_mismatch(
                &snapshot.profile_name,
                "server_epoch_identity",
                json!(reservation.state.epoch_id),
                json!(current.epoch_id),
            ));
        }
        if fs::symlink_metadata(&reservation.state.socket_path).is_ok() {
            verify_recorded_socket(&snapshot.profile_name, &reservation.state)?;
            fs::remove_file(&reservation.state.socket_path).map_err(io_error)?;
        }
        fs::remove_file(&paths.state_path).map_err(io_error)?;
    }
    sync_parent(&paths.state_path)?;
    if read_home_active(&paths.active_path)?.is_some() {
        clear_home_active(
            &snapshot.profile_name,
            &paths.active_path,
            &snapshot.server_key,
        )?;
    }
    drop(server_lock);
    drop(home_lock);
    Ok(())
}

/// Puts the account home back to `ready` after an APPLY that failed to stop
/// the server, so the record keeps telling the truth: the server is still up.
///
/// Best effort and token-guarded, for the same reason
/// [`release_reservation`] is.
fn restore_stopping_reservation(
    paths: &LifecyclePaths,
    snapshot: &ProfileSnapshot,
    reservation: &StopReservation,
) {
    let Ok(home_lock) = lock_file(&paths.home_root.join("home.lock")) else {
        return;
    };
    if require_transition(
        &paths.active_path,
        snapshot,
        reservation.state.server_epoch,
        reservation.token,
        "stopping",
    )
    .is_ok()
    {
        let _ = write_home_active(
            &paths.active_path,
            &HomeActive {
                schema_version: 1,
                canonical_codex_home: snapshot.canonical_codex_home.clone(),
                server_key: snapshot.server_key.clone(),
                server_epoch: reservation.state.server_epoch,
                lifecycle: "ready".to_owned(),
                pid: Some(reservation.state.pid),
                transition_token: None,
            },
        );
    }
    drop(home_lock);
}

fn verify_live_process_identity(state: &ServerState) -> Result<(), MachineError> {
    let expected = json!({
        "uid": state.uid,
        "process_group_id": state.pgid,
        "fingerprint": state.process_fingerprint,
    });
    let live = DarwinSystem.live_process_identity(state.pid).map_err(|_| {
        profile_mismatch(
            &state.snapshot.profile_name,
            "process_identity",
            expected.clone(),
            Value::Null,
        )
    })?;
    if live.uid != state.uid
        || live.process_group_id != state.pgid
        || live.fingerprint != state.process_fingerprint
    {
        return Err(profile_mismatch(
            &state.snapshot.profile_name,
            "process_identity",
            expected,
            json!({
                "uid": live.uid,
                "process_group_id": live.process_group_id,
                "fingerprint": live.fingerprint,
            }),
        ));
    }
    Ok(())
}

fn cleanup_failed_start(
    socket: &Path,
    process: &crate::darwin::LiveProcessIdentity,
    drainer: &crate::darwin::LiveProcessIdentity,
) {
    cleanup_verified_process(process);
    let _ = DarwinSystem.reap_child_nonblocking(process.process_group_id);
    cleanup_verified_process(drainer);
    if let Ok(metadata) = fs::symlink_metadata(socket)
        && metadata.file_type().is_socket()
        && metadata.uid() == DarwinSystem.current_uid()
    {
        let _ = fs::remove_file(socket);
    }
}

fn cleanup_verified_process(expected: &crate::darwin::LiveProcessIdentity) {
    let pid = expected.process_group_id;
    let verified = DarwinSystem
        .live_process_identity(pid)
        .is_ok_and(|live| live == *expected);
    if !verified {
        return;
    }
    let _ = DarwinSystem.signal_process_group(pid, libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_exists(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    if process_exists(pid)
        && DarwinSystem
            .live_process_identity(pid)
            .is_ok_and(|live| live == *expected)
    {
        let _ = DarwinSystem.signal_process_group(pid, libc::SIGKILL);
    }
}

fn stop_drainer(state: &ServerState) -> Result<(), MachineError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_exists(state.drainer_pid) && Instant::now() < deadline {
        if DarwinSystem.reap_child_nonblocking(state.drainer_pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !process_exists(state.drainer_pid) {
        return Ok(());
    }
    let live = DarwinSystem
        .live_process_identity(state.drainer_pid)
        .map_err(transport)?;
    if live.uid != state.drainer_uid
        || live.process_group_id != state.drainer_pgid
        || live.fingerprint != state.drainer_fingerprint
    {
        return Err(profile_mismatch(
            &state.snapshot.profile_name,
            "drainer_identity",
            json!({
                "uid": state.drainer_uid,
                "process_group_id": state.drainer_pgid,
                "fingerprint": state.drainer_fingerprint,
            }),
            json!({
                "uid": live.uid,
                "process_group_id": live.process_group_id,
                "fingerprint": live.fingerprint,
            }),
        ));
    }
    DarwinSystem
        .signal_process_group(state.drainer_pgid, libc::SIGTERM)
        .map_err(transport)
}

/// A profile server is "running" only when its recorded PID both exists and
/// still carries the exact identity recorded at start; a PID reused by an
/// unrelated process after a crash must never read back as running.
fn read_state_if_running(path: &Path) -> Result<Option<ServerState>, MachineError> {
    let Some(state) = read_state(path)? else {
        return Ok(None);
    };
    if process_identity_matches(state.pid, state.uid, state.pgid, &state.process_fingerprint) {
        Ok(Some(state))
    } else {
        Ok(None)
    }
}

fn read_state(path: &Path) -> Result<Option<ServerState>, MachineError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| MachineError::profile_config_invalid(path, error.to_string()))
}

fn process_exists(pid: u32) -> bool {
    DarwinSystem.process_exists(pid)
}

/// Revalidates a recorded process's full identity (uid, process group, and
/// executable fingerprint), not just PID existence, so a crashed process
/// whose PID a later, unrelated process reused is never mistaken for the
/// recorded one. Boot-UUID and full census-based revalidation are TASK-020.
fn process_identity_matches(pid: u32, uid: u32, pgid: u32, fingerprint: &str) -> bool {
    if !process_exists(pid) {
        return false;
    }
    DarwinSystem
        .live_process_identity(pid)
        .is_ok_and(|identity| {
            identity.uid == uid
                && identity.process_group_id == pgid
                && identity.fingerprint == fingerprint
        })
}

/// Proves a recorded server_key's process, drainer, and socket are all
/// absent — the "recorded lifetime absent" proof crash-recovery repair
/// (`profile state reset`, migration-fence repair) requires before acting.
fn server_lifetime_absent(context: &Context, server_key: &str) -> Result<bool, MachineError> {
    require_canonical_persisted_server_key(server_key)?;
    let root = context
        .application_support_root
        .join("profiles")
        .join(server_key);
    let state_path = root.join("state.json");
    if read_state_if_running(&state_path)?.is_some() {
        return Ok(false);
    }
    if let Some(state) = read_state(&state_path)?
        && (process_identity_matches(
            state.drainer_pid,
            state.drainer_uid,
            state.drainer_pgid,
            &state.drainer_fingerprint,
        ) || Path::new(&state.socket_path).exists())
    {
        return Ok(false);
    }
    Ok(true)
}

/// Operator repair for an unresolved migration fence: once both the old and
/// new recorded lifetimes are proven absent, the fence is released so a fresh
/// start or migration can proceed. This includes `migration_blocked`, whose
/// whole purpose is to require explicit reconciliation rather than selection.
/// Best-effort: an unprovable or irrelevant fence is left untouched rather
/// than failing the caller's otherwise-successful state reset.
fn repair_stale_migration_fence(
    context: &Context,
    home: &Path,
    server_key: &str,
) -> Result<bool, MachineError> {
    let migration_path = home.join("migration.json");
    if !migration_path.exists() {
        return Ok(false);
    }
    let (mut migration, parsed) = read_migration_record(&migration_path)?;
    if parsed.phase.is_terminal() || parsed.phase != MigrationPhase::Blocked {
        return Ok(false);
    }
    let old_key = parsed.old_server_key;
    let new_key = parsed.new_server_key;
    if old_key != server_key && new_key != server_key {
        return Ok(false);
    }
    for key in [&old_key, &new_key] {
        if key.is_empty() || !server_lifetime_absent(context, key)? {
            return Ok(false);
        }
    }
    migration["phase"] = Value::String("rolled_back".to_owned());
    migration["failure"] = Value::String("OPERATOR_STATE_RESET".to_owned());
    atomic_replace(
        &migration_path,
        &serde_json::to_vec_pretty(&migration).map_err(internal)?,
    )?;
    Ok(true)
}

/// Waits for the app-server to bind its Unix socket.
///
/// Both failures are `connect`-stage retryable transport failures: the socket
/// never came up, so no request of the caller's was written to it.
fn wait_for_socket(path: &Path, pid: u32) -> Result<(), MachineError> {
    let deadline = Instant::now() + SOCKET_BIND_BUDGET;
    while Instant::now() < deadline {
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
            return Ok(());
        }
        if !process_exists(pid) {
            return Err(transport_at(
                "connect",
                "app-server exited before binding its socket",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(transport_at(
        "connect",
        format!(
            "app-server did not bind its socket within {} seconds",
            SOCKET_BIND_BUDGET.as_secs()
        ),
    ))
}

fn probe_server(
    profile_name: &str,
    socket: &Path,
    expected_codex_home: &str,
) -> Result<ProbeResult, MachineError> {
    let mut connection =
        JsonRpcConnection::connect(socket, Duration::from_secs(30)).map_err(transport)?;
    let initialized = connection
        .request(
            "initialize",
            json!({
            "clientInfo": {"name": "dolgorae", "title": "Dolgorae", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": false, "optOutNotificationMethods": []}
        }),
        )
        .map_err(transport)?;
    if initialized["codexHome"].as_str() != Some(expected_codex_home) {
        return Err(profile_mismatch(
            profile_name,
            "canonical_codex_home",
            json!(expected_codex_home),
            initialized["codexHome"].clone(),
        ));
    }
    connection
        .notify("initialized", json!({}))
        .map_err(transport)?;
    let account = connection
        .request("account/read", json!({"refreshToken":false}))
        .map_err(transport)?;
    if !account["requiresOpenaiAuth"].is_boolean() {
        return Err(compatibility(
            profile_name,
            "app_server_probe",
            "account/read lacks requiresOpenaiAuth",
        ));
    }
    let mut cursor = Value::Null;
    let mut request_id = 3_u64;
    let mut models = BTreeSet::new();
    let mut default_count = 0_usize;
    let mut default_model = None;
    loop {
        let page = connection
            .request("model/list", json!({"cursor":cursor,"limit":100}))
            .map_err(transport)?;
        let data = page["data"].as_array().ok_or_else(|| {
            compatibility(
                profile_name,
                "app_server_probe",
                "model/list response lacks data",
            )
        })?;
        for model in data {
            let name = model["model"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    compatibility(
                        profile_name,
                        "app_server_probe",
                        "model/list item lacks model identity",
                    )
                })?;
            if !models.insert(name.to_owned()) {
                return Err(compatibility(
                    profile_name,
                    "app_server_probe",
                    "model/list returned a duplicate model",
                ));
            }
            if model["isDefault"].as_bool() == Some(true) {
                default_count += 1;
                default_model = Some(name.to_owned());
            }
            if !model["supportedReasoningEfforts"].is_array() {
                return Err(compatibility(
                    profile_name,
                    "app_server_probe",
                    "model/list item lacks reasoning efforts",
                ));
            }
        }
        cursor = page.get("nextCursor").cloned().unwrap_or(Value::Null);
        if cursor.is_null() {
            break;
        }
        if !cursor.is_string() || request_id >= 1000 {
            return Err(compatibility(
                profile_name,
                "app_server_probe",
                "model/list pagination is invalid or unbounded",
            ));
        }
        request_id += 1;
    }
    if models.is_empty() || default_count != 1 {
        return Err(compatibility(
            profile_name,
            "app_server_probe",
            "model/list must contain exactly one default model",
        ));
    }
    match connection.request(
        "thread/read",
        json!({"threadId": Uuid::now_v7(), "includeTurns": true}),
    ) {
        Err(TransportError::RemoteError { code: -32600, .. }) => {}
        Err(error) => {
            return Err(compatibility(
                profile_name,
                "app_server_probe",
                format!("absent-thread probe failed: {error}"),
            ));
        }
        Ok(_) => {
            return Err(compatibility(
                profile_name,
                "app_server_probe",
                "absent-thread probe unexpectedly succeeded",
            ));
        }
    }
    connection.close().map_err(transport)?;
    // Start from the closed, all-`Unverified` baseline and promote only the
    // names this probe actually exercised. `early_response_id` requires
    // proving the app-server's top-level `id` precedes a real streamed
    // `result` (SPEC-013's early-ID behavioral probe); no thread with real
    // content exists yet at bootstrap time to prove that against. The
    // transport exposes the most recently measured top-level `id` offset, but
    // an absent-thread error is not the required large successful-history
    // behavioral probe. It stays `Unverified` rather than
    // being claimed on the strength of the (necessary but insufficient)
    // absent-thread error check above. `native_subagent_lifecycle` likewise
    // stays `Unverified`: no run/thread exists at this bootstrap point to
    // observe a native subagent's lifecycle against.
    let mut capabilities = capability_snapshot(None);
    for name in [
        "account_read",
        "app_server_initialize",
        "model_list",
        "thread_absence_error",
    ] {
        capabilities.insert(name.to_owned(), ProfileCapabilityState::Supported);
    }
    Ok(ProbeResult {
        default_model: default_model.expect("exactly one default model"),
        models: models.into_iter().collect(),
        capabilities,
    })
}

fn profile_root(context: &Context, snapshot: &ProfileSnapshot) -> PathBuf {
    context
        .application_support_root
        .join("profiles")
        .join(&snapshot.server_key)
}

fn profile_state_path(context: &Context, snapshot: &ProfileSnapshot) -> PathBuf {
    profile_root(context, snapshot).join("state.json")
}

fn home_root(context: &Context, canonical_codex_home: &str) -> Result<PathBuf, MachineError> {
    let mut hasher = Sha256::new();
    hasher.update(b"dolgorae-home-v1\0");
    hasher.update(canonical_codex_home.as_bytes());
    Ok(context
        .application_support_root
        .join("homes")
        .join(format!("{:x}", hasher.finalize())))
}

fn read_home_active(path: &Path) -> Result<Option<HomeActive>, MachineError> {
    if !path.exists() {
        return Ok(None);
    }
    verify_private_regular_file(path, 0o600)?;
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| transport("home active contract is invalid"))
}

fn write_home_active(path: &Path, active: &HomeActive) -> Result<(), MachineError> {
    atomic_replace(path, &serde_json::to_vec_pretty(active).map_err(internal)?)
}

fn clear_home_active(
    profile_name: &str,
    path: &Path,
    server_key: &str,
) -> Result<(), MachineError> {
    let Some(active) = read_home_active(path)? else {
        return Ok(());
    };
    if active.server_key != server_key {
        return Err(profile_launch_conflict(
            profile_name,
            &active.server_key,
            format!("home active contract changed during a lifecycle operation on {server_key}"),
        ));
    }
    fs::remove_file(path).map_err(io_error)?;
    sync_parent(path)
}

fn socket_path(server_key: &str) -> Result<PathBuf, MachineError> {
    let uid = DarwinSystem.current_uid();
    let root = PathBuf::from(format!("/tmp/dolgorae-{uid}/p"));
    secure_dir(root.parent().expect("socket parent"))?;
    secure_dir(&root)?;
    let bytes = (0..40)
        .step_by(2)
        .map(|index| u8::from_str_radix(&server_key[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| transport("server key is not hexadecimal"))?;
    Ok(root.join(format!(
        "{}.sock",
        data_encoding::BASE32_NOPAD.encode(&bytes)
    )))
}

fn reserve_epoch(path: &Path) -> Result<u64, MachineError> {
    let current = if path.exists() {
        fs::read_to_string(path)
            .map_err(io_error)?
            .trim()
            .parse::<u64>()
            .map_err(|_| transport("profile epoch is invalid"))?
    } else {
        0
    };
    let next = current
        .checked_add(1)
        .ok_or_else(|| transport("profile epoch exhausted"))?;
    atomic_replace(path, format!("{next}\n").as_bytes())?;
    Ok(next)
}

fn append_membership(
    path: &Path,
    kind: &str,
    workspace_id: &str,
    run_id: Option<String>,
    scope: &MembershipScope,
) -> Result<u64, MachineError> {
    if path.exists() {
        verify_private_regular_file(path, 0o600)?;
    }
    let records = replay_membership(path, scope)?;
    let revision = records.last().map_or(1, |record| record.revision + 1);
    let previous = records
        .last()
        .map_or_else(|| "0".repeat(64), |record| record.record_sha256.clone());
    let body = json!({"schema_version":1,"revision":revision,"kind":kind,"workspace_id":workspace_id,"run_id":run_id,"previous_sha256":previous});
    let record = MembershipRecord {
        schema_version: 1,
        revision,
        kind: kind.to_owned(),
        workspace_id: workspace_id.to_owned(),
        run_id: body["run_id"].as_str().map(str::to_owned),
        previous_sha256: body["previous_sha256"].as_str().expect("string").to_owned(),
        record_sha256: canonical_sha256(&body)?,
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io_error)?;
    serde_json::to_writer(&mut file, &record).map_err(internal)?;
    file.write_all(b"\n").map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    sync_parent(path)?;
    let records = replay_membership(path, scope)?;
    let root = path
        .parent()
        .ok_or_else(|| transport("membership journal has no parent"))?;
    let index = derive_membership_index(root, &records)?;
    atomic_replace(
        &root.join("members.json"),
        &serde_json::to_vec_pretty(&index).map_err(internal)?,
    )?;
    Ok(revision)
}

fn derive_membership_index(
    root: &Path,
    records: &[MembershipRecord],
) -> Result<MembershipIndex, MachineError> {
    let mut active_members = BTreeMap::new();
    for record in records {
        let Some(run_id) = record.run_id.as_ref() else {
            continue;
        };
        let identity = format!("{}:{run_id}", record.workspace_id);
        match record.kind.as_str() {
            "run_registered" | "run_state_changed" => {
                active_members.insert(identity, record.clone());
            }
            "run_removed" | "run_closed" | "tombstone_orphan" | "run_migrated" => {
                active_members.remove(&identity);
            }
            _ => {}
        }
    }
    let journal = root.join("membership.jsonl");
    let journal_sha256 = if journal.exists() {
        file_sha256(&journal)?
    } else {
        sha256_hex(&[])
    };
    Ok(MembershipIndex {
        schema_version: 1,
        revision: records.last().map_or(0, |record| record.revision),
        journal_sha256,
        active_members,
    })
}

fn verify_membership_index(
    root: &Path,
    expected: &MembershipIndex,
    scope: &MembershipScope,
) -> Result<(), MachineError> {
    let path = root.join("members.json");
    if !path.exists() {
        if expected.revision == 0 {
            return Ok(());
        }
        return Err(scope.incomplete("derived membership index is missing"));
    }
    verify_private_regular_file(&path, 0o600)?;
    let actual: MembershipIndex = serde_json::from_slice(&fs::read(&path).map_err(io_error)?)
        .map_err(|_| transport("derived membership index is invalid"))?;
    if actual != *expected {
        return Err(scope.incomplete("derived membership index disagrees with the journal"));
    }
    Ok(())
}

fn membership_orphans(
    context: &Context,
    snapshot: &ProfileSnapshot,
    index: &MembershipIndex,
) -> Result<Vec<Value>, MachineError> {
    let mut orphans = Vec::new();
    for record in index.active_members.values() {
        let Some(run_id) = record.run_id.as_ref() else {
            continue;
        };
        let manifest_path = context
            .application_support_root
            .join("workspaces")
            .join(&record.workspace_id)
            .join("runs")
            .join(run_id)
            .join("manifest.json");
        let valid = fs::read(&manifest_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|manifest| {
                manifest["workspace_id"] == record.workspace_id
                    && manifest["run_id"] == *run_id
                    && manifest["profile"]["initial_server_key"] == snapshot.server_key
            });
        if !valid {
            orphans.push(json!({
                "workspace_id": record.workspace_id,
                "run_id": run_id,
                "reason": "run authority is absent or disagrees",
            }));
        }
    }
    Ok(orphans)
}

/// One Run recorded as a live member of a profile server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RunMember {
    pub workspace_id: String,
    pub run_id: String,
    pub revision: u64,
}

/// Why a Run stopped being a member of the server it joined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunMemberRelease {
    /// The Run reached a terminal state on this server.
    Closed,
    /// The Run moved to a different server key.
    Migrated,
}

impl RunMemberRelease {
    fn kind(self) -> &'static str {
        match self {
            Self::Closed => "run_closed",
            Self::Migrated => "run_migrated",
        }
    }
}

/// What a membership mutation committed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MembershipReceipt {
    pub server_key: String,
    pub revision: u64,
    pub live_members: Vec<RunMember>,
}

/// Records a Run as a live member of the profile server it is pinned to.
///
/// The membership journal already carries the vocabulary for this
/// (`run_registered`, `run_closed`, `run_migrated`, and the operator's
/// `tombstone_orphan`) and `profile server migrate` already refuses to move a
/// home out from under live members, but nothing could put a Run *into* the
/// journal: the only record any server ever wrote was its own
/// `server_started`. Every membership gate was therefore gating on a set that
/// was always empty. This is the registration side those gates were written
/// for, exposed as a callable so the Run lifecycle can hold it without the
/// profile module having to know how a Run starts.
///
/// Takes `home.lock` and then `server.lock`, matching lifecycle order. A new
/// registration is admitted only while this exact home contract is ready and
/// no migration fence is active; that makes the empty-membership proof and
/// stop reservation atomic against Run admission.
pub fn register_run_member(
    workspace: Option<&Path>,
    state: &ServerState,
    run_id: &str,
) -> Result<MembershipReceipt, MachineError> {
    record_membership(workspace, state, run_id, "run_registered")
}

/// Records that a Run is no longer a live member of its profile server.
///
/// The counterpart of [`register_run_member`]: until a Run releases its
/// membership, `profile server stop`, `restart`, and `migrate` all treat it as
/// a live member and refuse to interrupt it without explicit operator
/// evidence.
pub fn release_run_member(
    workspace: Option<&Path>,
    state: &ServerState,
    run_id: &str,
    reason: RunMemberRelease,
) -> Result<MembershipReceipt, MachineError> {
    record_membership(workspace, state, run_id, reason.kind())
}

/// Releases a Run using the immutable profile identity pinned in its manifest.
///
/// A closed Run no longer owns the live [`ServerState`] value that was used at
/// allocation time, but its manifest still pins the profile name and server
/// key. Resolve the current state under that exact key and then reuse the same
/// epoch-checked mutation path as [`release_run_member`].
pub fn release_run_member_by_identity(
    workspace: Option<&Path>,
    profile_name: &str,
    server_key: &str,
    run_id: &str,
    reason: RunMemberRelease,
) -> Result<MembershipReceipt, MachineError> {
    let context = context(workspace)?;
    let root = context
        .application_support_root
        .join("profiles")
        .join(server_key);
    verify_private_directory(&root)?;
    let state = read_state_if_running(&root.join("state.json"))?.ok_or_else(|| {
        profile_server_busy(
            profile_name,
            server_key,
            "the profile server this membership names is not running",
        )
    })?;
    if state.server_key != server_key || state.snapshot.profile_name != profile_name {
        return Err(profile_mismatch(
            profile_name,
            "membership_profile_identity",
            json!({"profile": profile_name, "server_key": server_key}),
            json!({
                "profile": state.snapshot.profile_name,
                "server_key": state.server_key,
            }),
        ));
    }
    record_membership(workspace, &state, run_id, reason.kind())
}

/// The Runs currently recorded as live members of a profile server.
///
/// Read under `server.lock` so a caller never observes a half-applied
/// registration, and verified against the derived index the journal commits
/// beside itself rather than trusting either copy alone.
pub fn live_run_members(
    workspace: Option<&Path>,
    state: &ServerState,
) -> Result<Vec<RunMember>, MachineError> {
    let context = context(workspace)?;
    let root = member_root(&context, state)?;
    let _server_lock = lock_file(&root.join("server.lock"))?;
    let scope = MembershipScope {
        profile: state.snapshot.profile_name.clone(),
        server_key: state.server_key.clone(),
    };
    let records = replay_membership(&root.join("membership.jsonl"), &scope)?;
    let index = derive_membership_index(&root, &records)?;
    verify_membership_index(&root, &index, &scope)?;
    Ok(run_members(&index))
}

fn record_membership(
    workspace: Option<&Path>,
    state: &ServerState,
    run_id: &str,
    kind: &str,
) -> Result<MembershipReceipt, MachineError> {
    if run_id.is_empty() || run_id.contains(':') || run_id.contains('/') {
        return Err(MachineError::invalid_argument(
            "run_id",
            "a membership run identity must be a nonempty path- and separator-free string",
        ));
    }
    let context = context(workspace)?;
    let root = member_root(&context, state)?;
    let home = home_root(&context, &state.snapshot.canonical_codex_home)?;
    verify_private_directory(&home)?;
    let _home_lock = lock_file(&home.join("home.lock"))?;
    let _server_lock = lock_file(&root.join("server.lock"))?;
    let state_path = root.join("state.json");
    // A membership record is only meaningful against the exact lifetime the
    // caller joined: a server that has been restarted underneath it is a
    // different epoch with a different journal position.
    let current = read_state_if_running(&state_path)?.ok_or_else(|| {
        profile_server_busy(
            &state.snapshot.profile_name,
            &state.server_key,
            "the profile server this membership names is not running",
        )
    })?;
    if current.epoch_id != state.epoch_id || current.server_key != state.server_key {
        return Err(profile_mismatch(
            &state.snapshot.profile_name,
            "server_epoch_identity",
            json!({"server_key": state.server_key, "epoch_id": state.epoch_id}),
            json!({"server_key": current.server_key, "epoch_id": current.epoch_id}),
        ));
    }
    if kind == "run_registered" {
        verify_migration_fence(
            &home.join("migration.json"),
            None,
            &state.snapshot.profile_name,
            &state.server_key,
        )?;
        let active = read_home_active(&home.join("active.json"))?;
        verify_stop_home_active(&state.snapshot, &current, active.as_ref())?;
    }
    let scope = MembershipScope {
        profile: state.snapshot.profile_name.clone(),
        server_key: state.server_key.clone(),
    };
    let journal = root.join("membership.jsonl");
    let revision = append_membership(
        &journal,
        kind,
        &context.workspace_id,
        Some(run_id.to_owned()),
        &scope,
    )?;
    // `stop_snapshot` refuses to act when the journal and the recorded server
    // state disagree about the revision, so the two move together or not at
    // all.
    let mut updated = current;
    updated.membership_revision = revision;
    atomic_replace(
        &state_path,
        &serde_json::to_vec_pretty(&updated).map_err(internal)?,
    )?;
    let index = derive_membership_index(&root, &replay_membership(&journal, &scope)?)?;
    Ok(MembershipReceipt {
        server_key: state.server_key.clone(),
        revision,
        live_members: run_members(&index),
    })
}

/// The profile directory a membership mutation works in, proven to be the one
/// the caller's own recorded launch contract names.
fn member_root(context: &Context, state: &ServerState) -> Result<PathBuf, MachineError> {
    let root = context
        .application_support_root
        .join("profiles")
        .join(&state.server_key);
    let recorded = path_utf8(&root)?;
    if recorded != state.snapshot.derived_launch_cwd {
        return Err(profile_mismatch(
            &state.snapshot.profile_name,
            "derived_launch_cwd",
            json!(state.snapshot.derived_launch_cwd),
            json!(recorded),
        ));
    }
    verify_private_directory(&root)?;
    Ok(root)
}

fn run_members(index: &MembershipIndex) -> Vec<RunMember> {
    index
        .active_members
        .values()
        .filter_map(|record| {
            record.run_id.as_ref().map(|run_id| RunMember {
                workspace_id: record.workspace_id.clone(),
                run_id: run_id.clone(),
                revision: record.revision,
            })
        })
        .collect()
}

/// The profile a membership journal belongs to.
///
/// `PROFILE_MEMBERSHIP_INCOMPLETE` names the profile, its server key, and the
/// repair reason; a journal pathname alone proves none of the first two, so
/// every reader of the journal carries them.
#[derive(Clone, Debug)]
struct MembershipScope {
    profile: String,
    server_key: String,
}

impl MembershipScope {
    fn incomplete(&self, reason: impl Into<String>) -> MachineError {
        profile_membership_incomplete(&self.profile, &self.server_key, reason)
    }
}

fn replay_membership(
    path: &Path,
    scope: &MembershipScope,
) -> Result<Vec<MembershipRecord>, MachineError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path).map_err(io_error)?;
    if bytes.len() as u64 > MAX_DIAGNOSTIC_BYTES {
        return Err(transport("membership journal exceeds 8 MiB"));
    }
    let mut records = Vec::new();
    let mut previous = "0".repeat(64);
    for (index, line) in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let record: MembershipRecord = serde_json::from_slice(line).map_err(|_| {
            scope.incomplete(format!("membership journal line {} is invalid", index + 1))
        })?;
        let body = json!({"schema_version":record.schema_version,"revision":record.revision,"kind":record.kind,"workspace_id":record.workspace_id,"run_id":record.run_id,"previous_sha256":record.previous_sha256});
        if record.revision != u64::try_from(index + 1).expect("bounded")
            || record.previous_sha256 != previous
            || record.record_sha256 != canonical_sha256(&body)?
        {
            return Err(scope.incomplete(format!(
                "membership hash chain breaks at line {}",
                index + 1
            )));
        }
        previous = record.record_sha256.clone();
        records.push(record);
    }
    Ok(records)
}

/// Drop the `details` of a record the operator gate owns from a projection
/// that does not require the operator capability.
///
/// Records written before the projection marker existed are unmarked and keep
/// their historical shape, so this narrows exposure without rewriting history.
fn project_diagnostic(record: Value, projection: &str) -> Value {
    if projection == DiagnosticProjection::Operational.as_str() {
        return record;
    }
    let operational = record
        .get("projection")
        .and_then(Value::as_str)
        .is_some_and(|marker| marker == DiagnosticProjection::Operational.as_str());
    if !operational {
        return record;
    }
    let Value::Object(mut members) = record else {
        return record;
    };
    members.remove("details");
    Value::Object(members)
}

fn append_diagnostic(root: &Path, kind: &str, details: Value) -> Result<(), MachineError> {
    append_diagnostic_record(root, kind, details, DiagnosticProjection::Minimal)
}

/// Which `profile events` / `profile diagnostics list` projection may show a
/// record's `details`.
///
/// SPEC-006 keeps verified identities, foreign-thread routing metadata, and
/// bounded transport detail in the operational projection, which requires the
/// operator capability; the minimal projection carries only the record's
/// identity and kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticProjection {
    Minimal,
    Operational,
}

impl DiagnosticProjection {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Operational => "operational",
        }
    }
}

/// Append one bounded record to a Runtime Profile's durable diagnostic
/// journal.
///
/// This is the authority a foreign-thread observation is recorded in:
/// specs.md closes the v1 Run audit-kind enum and states that a foreign-thread
/// diagnostic "is never a Run event and uses the separate profile diagnostic
/// schema", so a Run ledger record is not an option for it.
pub fn append_profile_diagnostic(
    profile_root: &Path,
    kind: &str,
    details: Value,
    projection: DiagnosticProjection,
) -> Result<(), MachineError> {
    secure_dir(profile_root)?;
    append_diagnostic_record(profile_root, kind, details, projection)
}

fn append_diagnostic_record(
    root: &Path,
    kind: &str,
    details: Value,
    projection: DiagnosticProjection,
) -> Result<(), MachineError> {
    let path = root.join("diagnostics.jsonl");
    if path.exists() {
        verify_private_regular_file(&path, 0o600)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(io_error)?;
    let value = json!({
        "schema_version": 1,
        "diagnostic_id": Uuid::now_v7(),
        "kind": kind,
        "projection": projection.as_str(),
        "details": details,
    });
    let line = serde_json::to_vec(&value).map_err(internal)?;
    if line.len() > MAX_DIAGNOSTIC_RECORD_BYTES {
        return Err(MachineError::invalid_argument(
            "details",
            "diagnostic record exceeds its bound",
        ));
    }
    file.write_all(&line).map_err(io_error)?;
    file.write_all(b"\n").map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    Ok(())
}

fn lock_file(path: &Path) -> Result<File, MachineError> {
    if path.exists() {
        verify_private_regular_file(path, 0o600)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(io_error)?;
    verify_private_regular_file(path, 0o600)?;
    DarwinSystem.lock_exclusive(&file).map_err(io_error)?;
    Ok(file)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), MachineError> {
    let parent = path
        .parent()
        .ok_or_else(|| transport("path has no parent"))?;
    secure_dir(parent)?;
    if path.exists() {
        verify_private_regular_file(path, 0o600)?;
    }
    let temporary = parent.join(format!(".tmp-{}", Uuid::now_v7()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    fs::rename(&temporary, path).map_err(io_error)?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> Result<(), MachineError> {
    File::open(
        path.parent()
            .ok_or_else(|| transport("path has no parent"))?,
    )
    .and_then(|file| file.sync_all())
    .map_err(io_error)
}

fn secure_dir(path: &Path) -> Result<(), MachineError> {
    if path.exists() {
        return verify_private_directory(path);
    }
    let parent = path
        .parent()
        .ok_or_else(|| MachineError::runtime_path_invalid(path, "directory has no parent"))?;
    if !parent.exists() {
        secure_dir(parent)?;
    }
    fs::create_dir(path).map_err(io_error)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    verify_private_directory(path)
}

fn verify_private_directory(path: &Path) -> Result<(), MachineError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(MachineError::runtime_path_invalid(
            path,
            "directory must be current-uid-owned mode 0700",
        ));
    }
    Ok(())
}

fn verify_private_regular_file(path: &Path, mode: u32) -> Result<(), MachineError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.mode() & 0o777 != mode
    {
        return Err(MachineError::runtime_path_invalid(
            path,
            format!("file must be current-uid-owned mode {mode:04o}"),
        ));
    }
    Ok(())
}

fn read_json(profile_name: &str, path: &Path) -> Result<Value, MachineError> {
    let bytes = fs::read(path).map_err(|_| {
        compatibility(
            profile_name,
            "required_schema_file",
            format!("required schema file is missing: {}", path.display()),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        compatibility(
            profile_name,
            "required_schema_file",
            format!("required schema file is invalid: {}", path.display()),
        )
    })
}

fn directory_sha256(profile_name: &str, root: &Path) -> Result<String, MachineError> {
    let mut files = Vec::new();
    collect_files(profile_name, root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    hasher.update(b"dolgorae-schema-bundle-v1\0");
    for (relative, path) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        let bytes = fs::read(path).map_err(io_error)?;
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_files(
    profile_name: &str,
    root: &Path,
    current: &Path,
    output: &mut Vec<(String, PathBuf)>,
) -> Result<(), MachineError> {
    for entry in fs::read_dir(current).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(io_error)?;
        if metadata.is_dir() {
            collect_files(profile_name, root, &path, output)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| transport("schema path escaped root"))?;
            output.push((path_utf8(relative)?.to_owned(), path));
        } else {
            return Err(compatibility(
                profile_name,
                "schema_bundle_files",
                "schema bundle contains a non-regular entry",
            ));
        }
    }
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, MachineError> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn canonical_sha256(value: &Value) -> Result<String, MachineError> {
    let text = serde_json::to_string(value).map_err(internal)?;
    let parsed = parse(&text).map_err(|error| internal(error.to_string()))?;
    let bytes = canonicalize(&parsed).map_err(|error| internal(error.to_string()))?;
    Ok(sha256_hex(&bytes))
}

fn domain_separated_sha256(domain: &[u8], value: &Value) -> Result<String, MachineError> {
    let text = serde_json::to_string(value).map_err(internal)?;
    let parsed = parse(&text).map_err(|error| internal(error.to_string()))?;
    let bytes = canonicalize(&parsed).map_err(|error| internal(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn path_utf8(path: &Path) -> Result<&str, MachineError> {
    path.to_str().ok_or_else(|| {
        MachineError::profile_config_invalid(path, "TASK-005 profile paths must be UTF-8")
    })
}

fn profile_not_found(name: &str) -> MachineError {
    MachineError::new(
        "PROFILE_NOT_FOUND",
        "profile not found",
        false,
        json!({"profile": name}),
    )
}

/// A checked `PROFILE_MISMATCH`: the addressed profile's recorded identity
/// disagrees with what was expected on one named field. `expected`/`actual`
/// may be composite objects when more than one underlying value backs the
/// same logical comparison (e.g. a whole recorded process identity).
fn profile_mismatch(profile: &str, field: &str, expected: Value, actual: Value) -> MachineError {
    MachineError::new(
        "PROFILE_MISMATCH",
        "profile identity does not match the recorded contract",
        false,
        json!({"profile": profile, "field": field, "expected": expected, "actual": actual}),
    )
}

/// A checked `PROFILE_LAUNCH_CONFLICT`: the canonical `CODEX_HOME` already
/// has a different active launch contract.
fn profile_launch_conflict(
    profile: &str,
    server_key: &str,
    reason: impl Into<String>,
) -> MachineError {
    MachineError::new(
        "PROFILE_LAUNCH_CONFLICT",
        "another launch contract owns this account home",
        false,
        json!({"profile": profile, "server_key": server_key, "reason": reason.into()}),
    )
}

/// A checked, retryable `PROFILE_SERVER_BUSY`: a serialized profile
/// operation or active-member condition temporarily blocks the command.
fn profile_server_busy(profile: &str, server_key: &str, reason: impl Into<String>) -> MachineError {
    MachineError::new(
        "PROFILE_SERVER_BUSY",
        "profile server is temporarily busy",
        true,
        json!({"profile": profile, "server_key": server_key, "reason": reason.into()}),
    )
}

/// A checked `PROFILE_MEMBERSHIP_INCOMPLETE`: the manager cannot prove its
/// durable membership journal complete for the named repair action.
fn profile_membership_incomplete(
    profile: &str,
    server_key: &str,
    reason: impl Into<String>,
) -> MachineError {
    MachineError::new(
        "PROFILE_MEMBERSHIP_INCOMPLETE",
        "profile membership is not provably complete",
        false,
        json!({"profile": profile, "server_key": server_key, "reason": reason.into()}),
    )
}

/// A checked `OPERATOR_MISMATCH` naming which operation the absent, stale,
/// or invalid operator credential failed to authorize.
fn operator_mismatch(operation: &str) -> MachineError {
    MachineError::new(
        "OPERATOR_MISMATCH",
        "operator credential does not authorize this operation",
        false,
        json!({"operation": operation}),
    )
}

/// Builds the closed profile-specific Codex capability snapshot `profile
/// doctor` and `profile show` report: every name in `PROFILE_CAPABILITY_NAMES`
/// is always present, taken from `stored` when it has that name and
/// `Unverified` otherwise.
fn capability_snapshot(
    stored: Option<&BTreeMap<String, ProfileCapabilityState>>,
) -> BTreeMap<String, ProfileCapabilityState> {
    PROFILE_CAPABILITY_NAMES
        .iter()
        .map(|name| {
            let state = stored
                .and_then(|map| map.get(*name))
                .copied()
                .unwrap_or(ProfileCapabilityState::Unverified);
            ((*name).to_owned(), state)
        })
        .collect()
}

/// A checked `COMPATIBILITY_REJECTED`: one named check against the pinned
/// Codex release rejected what it observed for one profile.
///
/// The contract closes the details to `{profile, check, expected, actual}`, so
/// the observation travels as `actual` and the pinned requirement as
/// `expected`; a free-text reason with no subject is not a member the contract
/// has.
fn compatibility(profile: &str, check: &str, observed: impl Into<String>) -> MachineError {
    MachineError::new(
        "COMPATIBILITY_REJECTED",
        "Codex compatibility check failed",
        false,
        json!({
            "profile": profile,
            "check": check,
            "expected": format!("conformance with pinned Codex {SUPPORTED_CODEX_VERSION}"),
            "actual": observed.into(),
        }),
    )
}
/// A checked retryable `TRANSPORT_FAILURE`.
///
/// Every profile transport here is a probe made before any Run exists, so
/// nothing the caller asked for can have been written; that is what makes it
/// retryable, and it is the acceptance the contract requires it to state.
fn transport(error: impl std::fmt::Display) -> MachineError {
    transport_at("read", error)
}

/// The same checked retryable `TRANSPORT_FAILURE` for a caller that knows
/// which contract stage it failed at.
///
/// `stage` is one of the closed `d_transport_retryable` stages
/// (`connect`, `write`, `read`, `decode`, `correlate`, `shutdown`).
/// `OPERATION_TIMEOUT` is not an alternative here: the error contract closes
/// its `operation` to `replay`, `schema_generation`, and `profile_doctor`,
/// and pins its `retryable` to `true`, so a non-retryable `OPERATION_TIMEOUT`
/// carrying a `timeout_seconds` detail is not a shape the contract has.
/// A bind or connect that expired wrote nothing, so it is exactly the
/// retryable transport failure the contract does define.
fn transport_at(stage: &'static str, error: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "TRANSPORT_FAILURE",
        error.to_string(),
        true,
        json!({"stage": stage, "acceptance": "not_written", "request_id": Value::Null}),
    )
}
fn io_error(error: std::io::Error) -> MachineError {
    transport(error)
}
fn internal(error: impl std::fmt::Display) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "internal profile operation failed",
        false,
        json!({"invariant": error.to_string()}),
    )
}

#[derive(Clone, Debug)]
struct Parsed {
    positionals: Vec<String>,
    values: BTreeMap<String, Vec<String>>,
    flags: BTreeSet<String>,
    trailing: Vec<String>,
}

impl Parsed {
    fn new(arguments: &[OsString], operation: ProfileOperation) -> Result<Self, MachineError> {
        let mut parsed = Self {
            positionals: Vec::new(),
            values: BTreeMap::new(),
            flags: BTreeSet::new(),
            trailing: Vec::new(),
        };
        let mut index = 0;
        while index < arguments.len() {
            let token = arguments[index].to_str().ok_or_else(|| {
                MachineError::invalid_argument("argv", "profile arguments must be UTF-8")
            })?;
            if token == "--" {
                if operation != ProfileOperation::Add {
                    return Err(MachineError::invalid_argument(
                        "argv",
                        "only profile add accepts executable argv",
                    ));
                }
                parsed.trailing = arguments[index + 1..]
                    .iter()
                    .map(|value| {
                        value.to_str().map(str::to_owned).ok_or_else(|| {
                            MachineError::invalid_argument("argv", "profile argv must be UTF-8")
                        })
                    })
                    .collect::<Result<_, _>>()?;
                break;
            }
            if token.contains('=') && token.starts_with("--") {
                return Err(MachineError::invalid_argument(
                    token,
                    "equals-form options are not accepted",
                ));
            }
            if operation.allows_flag(token) {
                if !parsed.flags.insert(token.to_owned()) {
                    return Err(MachineError::invalid_argument(
                        token,
                        "option may appear only once",
                    ));
                }
                index += 1;
                continue;
            }
            if token.starts_with("--") {
                if !operation.allows_value(token) {
                    return Err(MachineError::invalid_argument(token, "unknown option"));
                }
                let value = arguments
                    .get(index + 1)
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| {
                        MachineError::invalid_argument(token, "option value is missing")
                    })?;
                parsed
                    .values
                    .entry(token.to_owned())
                    .or_default()
                    .push(value.to_owned());
                index += 2;
                continue;
            }
            parsed.positionals.push(token.to_owned());
            index += 1;
        }
        operation.validate_shape(&parsed)?;
        Ok(parsed)
    }
    fn required_positional(&self, index: usize, label: &str) -> Result<String, MachineError> {
        self.positionals
            .get(index)
            .cloned()
            .ok_or_else(|| MachineError::invalid_argument(label, "required value is missing"))
    }
    fn required(&self, name: &str) -> Result<String, MachineError> {
        self.values
            .get(name)
            .and_then(|values| values.last())
            .cloned()
            .ok_or_else(|| MachineError::invalid_argument(name, "required option is missing"))
    }
    fn path(&self, name: &str) -> Result<Option<PathBuf>, MachineError> {
        Ok(self
            .values
            .get(name)
            .and_then(|values| values.last())
            .map(PathBuf::from))
    }
    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
    fn environment(&self) -> Result<BTreeMap<String, String>, MachineError> {
        let mut result = BTreeMap::new();
        for entry in self.values.get("--env").into_iter().flatten() {
            let (name, value) = entry.split_once('=').ok_or_else(|| {
                MachineError::invalid_argument("--env", "environment entries use NAME=VALUE")
            })?;
            if name.is_empty()
                || value.is_empty()
                || result.insert(name.to_owned(), value.to_owned()).is_some()
            {
                return Err(MachineError::invalid_argument(
                    "--env",
                    "environment names must be unique and nonempty",
                ));
            }
        }
        for required in ["PATH", "LANG", "LC_ALL"] {
            if !result.contains_key(required) {
                return Err(MachineError::invalid_argument(
                    "--env",
                    format!("missing required {required}"),
                ));
            }
        }
        Ok(result)
    }
    /// Rejects an absent or stale operator credential early, before the
    /// command spends any work on profile discovery or compatibility
    /// probing. A malformed carrier (both `--operator-file` and
    /// `--operator-fd`, or neither) surfaces as `INVALID_ARGUMENT` straight
    /// from `carrier_from_options`, exactly as it would for any other
    /// checked command; only a well-formed carrier that fails authorization
    /// becomes the profile operator error, `OPERATOR_MISMATCH`.
    ///
    /// This is deliberately *not* the authorizing check: it releases
    /// `operator.lock` at once, and SPEC-013 says such a pre-lock check can
    /// never authorize an operation across a concurrent rotation. Every
    /// effect re-authorizes through [`Parsed::authorize_operator`] and holds
    /// the lock until its handoff boundary.
    fn precheck_operator(&self, operation: &str) -> Result<(), MachineError> {
        self.authorize_operator(operation)
            .map(OperatorHandoff::release)
    }

    /// Authorizes the operator credential and keeps `operator.lock` held for
    /// the effect that follows.
    ///
    /// Call this immediately before the effect, never before discovery or a
    /// compatibility probe: no operation may wait on an external process
    /// while it holds a filesystem lock.
    fn authorize_operator(&self, operation: &str) -> Result<OperatorHandoff, MachineError> {
        let carrier = self.operator_carrier()?;
        let store =
            crate::controller::OperatorStore::new(crate::controller::default_operator_root()?);
        authorize_operator_in(&store, &carrier, operation)
    }

    fn operator_carrier(&self) -> Result<crate::controller::CredentialCarrier, MachineError> {
        let mut arguments = Vec::new();
        for flag in ["--operator-file", "--operator-fd"] {
            if let Some(value) = self.values.get(flag).and_then(|values| values.last()) {
                arguments.push(OsString::from(flag));
                arguments.push(OsString::from(value));
            }
        }
        crate::controller::carrier_from_options(&arguments, "--operator-file", "--operator-fd")
    }
    fn reject_positionals_and_trailing(&self) -> Result<(), MachineError> {
        if self.positionals.is_empty() && self.trailing.is_empty() {
            Ok(())
        } else {
            Err(MachineError::invalid_argument(
                "argv",
                "unexpected positional argument",
            ))
        }
    }
}

/// Authorizes `carrier` against `store` and wraps the resulting hold for the
/// effect to carry. Split out from [`Parsed::authorize_operator`] so a test
/// can drive a store rooted in a temporary directory instead of the real
/// user-private operator root.
fn authorize_operator_in(
    store: &crate::controller::OperatorStore,
    carrier: &crate::controller::CredentialCarrier,
    operation: &str,
) -> Result<OperatorHandoff, MachineError> {
    store
        .authorize(carrier)
        .map(OperatorHandoff::held)
        .map_err(|_| operator_mismatch(operation))
}

/// Carries operator authorization into an operator-authorized effect.
///
/// SPEC-013 and ADR-016 put `operator.lock` at the top of the one global
/// acquisition hierarchy and require the credential reread and constant-time
/// comparison to happen under it *before* the effect takes the next lock or
/// causes any change. Passing this into the effect instead of authorizing and
/// returning is what closes the rotation race: the hold survives the whole
/// gap between the comparison and the effect, so a rotation racing the
/// operation either finishes first — and the authorization then fails — or
/// waits until the effect has reached its handoff boundary.
#[derive(Debug)]
struct OperatorHandoff(Option<crate::controller::OperatorAuthorization>);

impl OperatorHandoff {
    /// No hold to carry. Either the operation is not operator-authorized at
    /// all (`profile server start`, the doctor probe's own cleanup stop), or
    /// it is the compensating rollback of an authorized operation already in
    /// flight, which a rotation landing mid-flight must not be able to
    /// strand.
    fn none() -> Self {
        Self(None)
    }

    /// Carries an authorization whose hold has to survive until `handoff`.
    fn held(authorization: crate::controller::OperatorAuthorization) -> Self {
        Self(Some(authorization))
    }

    /// Releases `operator.lock` now that `_next` — the home, server, or run
    /// lock that follows it in the normative order — is held. That handoff is
    /// the linearization point of the authorized operation: a rotation may
    /// proceed from here, but it can no longer overtake an effect that has
    /// already begun under the next lock.
    ///
    /// Taking the next lock by reference makes the boundary un-skippable:
    /// there is no way to release the hold without naming the lock that
    /// replaces it.
    fn handoff(&mut self, _next: &File) {
        if let Some(authorization) = self.0.take() {
            authorization.release();
        }
    }

    /// Releases `operator.lock` at the end of an effect that acquires no
    /// further lock, so the hold spans exactly its prepare and commit.
    fn release(self) {
        if let Some(authorization) = self.0 {
            authorization.release();
        }
    }
}

impl ProfileOperation {
    fn allows_flag(self, name: &str) -> bool {
        matches!(
            (self, name),
            (Self::Doctor, "--launch-probe" | "--leave-running")
                | (
                    Self::ServerStop | Self::ServerRestart | Self::ServerMigrate,
                    "--interrupt"
                )
                | (Self::StateReset, "--require-server-absence")
                | (Self::Events, "--follow")
        )
    }

    fn allows_value(self, name: &str) -> bool {
        if name == "--workspace" {
            return true;
        }
        match self {
            Self::Add => matches!(name, "--codex-home" | "--native-subagents" | "--env"),
            Self::ServerStop | Self::ServerRestart => matches!(
                name,
                "--operator-file" | "--operator-fd" | "--confirm-server-key"
            ),
            Self::ServerMigrate => matches!(
                name,
                "--operator-file"
                    | "--operator-fd"
                    | "--confirm-old-server-key"
                    | "--confirm-new-server-key"
            ),
            Self::MembershipTombstoneOrphan => matches!(
                name,
                "--operator-file"
                    | "--operator-fd"
                    | "--confirm-server-key"
                    | "--confirm-workspace-id"
                    | "--confirm-run-id"
            ),
            Self::StateReset => matches!(
                name,
                "--operator-file" | "--operator-fd" | "--confirm-server-key"
            ),
            Self::DiagnosticsList => matches!(
                name,
                "--after" | "--limit" | "--projection" | "--operator-file" | "--operator-fd"
            ),
            Self::Events => matches!(
                name,
                "--after" | "--projection" | "--operator-file" | "--operator-fd"
            ),
            Self::List
            | Self::Show
            | Self::Remove
            | Self::Doctor
            | Self::ServerStatus
            | Self::ServerStart
            | Self::MembershipVerify => false,
        }
    }

    fn validate_shape(self, parsed: &Parsed) -> Result<(), MachineError> {
        let expected_positionals = usize::from(self != Self::List);
        if parsed.positionals.len() != expected_positionals {
            return Err(MachineError::invalid_argument(
                "argv",
                format!("expected {expected_positionals} positional profile names"),
            ));
        }
        if self == Self::Add {
            if parsed.trailing.is_empty() {
                return Err(MachineError::invalid_argument(
                    "argv",
                    "profile add requires a direct executable argv after --",
                ));
            }
        } else if !parsed.trailing.is_empty() {
            return Err(MachineError::invalid_argument(
                "argv",
                "unexpected executable argv",
            ));
        }
        for (name, values) in &parsed.values {
            if name != "--env" && values.len() != 1 {
                return Err(MachineError::invalid_argument(
                    name,
                    "option may appear only once",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_operational_diagnostic_keeps_its_details_out_of_the_minimal_projection() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-diagnostic-projection-{}", Uuid::now_v7()));
        append_profile_diagnostic(
            &root,
            "foreign_thread_request_ignored",
            json!({"thread_id": "thread-other"}),
            DiagnosticProjection::Operational,
        )
        .unwrap();
        append_diagnostic(&root, "server_ready", json!({"pid": 42})).unwrap();

        let bytes = fs::read(root.join("diagnostics.jsonl")).unwrap();
        let records: Vec<Value> = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);

        // Operational detail is gated on the operator capability; the minimal
        // projection keeps the record's identity and drops the detail.
        let minimal = project_diagnostic(records[0].clone(), "minimal");
        assert_eq!(minimal["kind"], "foreign_thread_request_ignored");
        assert_eq!(minimal["diagnostic_id"], records[0]["diagnostic_id"]);
        assert!(minimal.get("details").is_none());
        assert_eq!(
            project_diagnostic(records[0].clone(), "operational")["details"]["thread_id"],
            "thread-other"
        );

        // A record written before the marker existed keeps its historical
        // shape rather than being retroactively redacted.
        let unmarked = project_diagnostic(
            json!({"schema_version":1,"kind":"server_ready","details":{"pid":42}}),
            "minimal",
        );
        assert_eq!(unmarked["details"]["pid"], 42);
        assert_eq!(
            project_diagnostic(records[1].clone(), "minimal")["details"]["pid"],
            42
        );

        fs::remove_dir_all(&root).unwrap();
    }

    use super::*;

    #[test]
    fn version_policy_is_exact_tested_and_newer_unverified() {
        assert_eq!(
            version_verdict("default", "0.149.0").unwrap(),
            CompatibilityVerdict::Tested
        );
        assert_eq!(
            version_verdict("default", "0.150.0").unwrap(),
            CompatibilityVerdict::Unverified
        );
        assert_eq!(
            version_verdict("default", "0.148.9").unwrap_err().code,
            "COMPATIBILITY_REJECTED"
        );
    }

    #[test]
    fn parser_keeps_direct_argv_after_separator() {
        let args = [
            "demo",
            "--codex-home",
            "/tmp/codex",
            "--native-subagents",
            "enabled",
            "--env",
            "PATH=/bin",
            "--env",
            "LANG=C",
            "--env",
            "LC_ALL=C",
            "--",
            "/bin/codex",
            "--strict-config",
        ]
        .map(OsString::from);
        let parsed = Parsed::new(&args, ProfileOperation::Add).unwrap();
        assert_eq!(parsed.positionals, ["demo"]);
        assert_eq!(parsed.trailing, ["/bin/codex", "--strict-config"]);
        assert_eq!(parsed.environment().unwrap().len(), 3);
    }

    #[test]
    fn membership_chain_rejects_rewritten_history() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-membership-test-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();
        let path = root.join("membership.jsonl");
        let scope = MembershipScope {
            profile: "default".to_owned(),
            server_key: "a".repeat(64),
        };
        append_membership(&path, "server_started", "workspace", None, &scope).unwrap();
        append_membership(
            &path,
            "tombstone_orphan",
            "workspace",
            Some("run".to_owned()),
            &scope,
        )
        .unwrap();
        assert_eq!(replay_membership(&path, &scope).unwrap().len(), 2);
        let mut bytes = fs::read(&path).unwrap();
        bytes[20] ^= 1;
        fs::write(&path, bytes).unwrap();
        let broken = replay_membership(&path, &scope).unwrap_err();
        assert_eq!(broken.code, "PROFILE_MEMBERSHIP_INCOMPLETE");
        // The checked contract requires every member of `d_profile_server`.
        assert_eq!(broken.details["profile"], "default");
        assert_eq!(broken.details["server_key"], "a".repeat(64));
        assert!(broken.details["reason"].is_string());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configuration_classification_is_closed_and_separates_runtime_observation() {
        let root = std::env::temp_dir().join(format!("dolgorae-config-test-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();
        let profile = RuntimeProfile {
            argv: vec!["/bin/codex".to_owned()],
            codex_home: root.to_string_lossy().into_owned(),
            environment: BTreeMap::new(),
            native_subagents: NativeSubagents::Enabled,
        };
        let config = root.join("config.toml");
        fs::write(
            &config,
            "approval_policy = \"never\"\nmodel = \"gpt-5\"\napprovals_reviewer = \"user\"\n[desktop]\nfollowUpQueueMode = \"queue\"\n",
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let snapshot = configuration_snapshot("default", &profile, &profile.codex_home).unwrap();
        assert_eq!(snapshot.launch["model"], "gpt-5");
        assert_eq!(snapshot.launch["approvals_reviewer"], "user");
        assert_eq!(snapshot.observation["approval_policy"], "never");
        assert_eq!(
            snapshot.observation["desktop"]["followUpQueueMode"],
            "queue"
        );
        fs::write(&config, "unknown_future_field = true\n").unwrap();
        assert_eq!(
            configuration_snapshot("default", &profile, &profile.codex_home)
                .unwrap_err()
                .code,
            "COMPATIBILITY_REJECTED"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_arguments_checks_shape_without_executing() {
        assert!(validate_arguments(ProfileOperation::List, &[]).is_ok());
        let bogus = [OsString::from("--bogus")];
        assert_eq!(
            validate_arguments(ProfileOperation::List, &bogus)
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn operator_carrier_conflict_preserves_invalid_argument() {
        let args = [
            "demo",
            "--operator-file",
            "/tmp/a",
            "--operator-fd",
            "3",
            "--confirm-server-key",
            &"a".repeat(64),
            "--require-server-absence",
        ]
        .map(OsString::from);
        let parsed = Parsed::new(&args, ProfileOperation::StateReset).unwrap();
        let error = parsed.precheck_operator("profile.state.reset").unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert_eq!(error.details["argument"], "--operator-file");
    }

    #[test]
    fn operator_carrier_absent_is_invalid_argument_not_operator_mismatch() {
        let args = [
            "demo",
            "--confirm-server-key",
            &"a".repeat(64),
            "--require-server-absence",
        ]
        .map(OsString::from);
        let parsed = Parsed::new(&args, ProfileOperation::StateReset).unwrap();
        let error = parsed.precheck_operator("profile.state.reset").unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENT");
    }

    #[test]
    fn operator_carrier_well_formed_but_unauthorized_is_operator_mismatch() {
        // A carrier that opens cleanly (right owner/mode/size) but whose
        // content is not a real operator credential must fail authorization,
        // not carrier parsing, so this exercises the OPERATOR_MISMATCH path
        // distinctly from the INVALID_ARGUMENT carrier-shape tests above.
        let root = std::env::temp_dir().join(format!("dolgorae-operator-test-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();
        let bogus_credential = root.join("bogus-operator.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&bogus_credential)
            .unwrap();
        file.write_all(b"not a real operator credential").unwrap();
        drop(file);
        let args = [
            "demo",
            "--operator-file",
            bogus_credential.to_str().unwrap(),
            "--confirm-server-key",
            &"a".repeat(64),
            "--require-server-absence",
        ]
        .map(OsString::from);
        let parsed = Parsed::new(&args, ProfileOperation::StateReset).unwrap();
        let error = parsed.precheck_operator("profile.state.reset").unwrap_err();
        assert_eq!(error.code, "OPERATOR_MISMATCH");
        assert_eq!(error.details["operation"], "profile.state.reset");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn capability_snapshot_is_closed_and_defaults_to_unverified() {
        let empty = capability_snapshot(None);
        assert_eq!(empty.len(), PROFILE_CAPABILITY_NAMES.len());
        for name in PROFILE_CAPABILITY_NAMES {
            assert_eq!(empty[name], ProfileCapabilityState::Unverified);
        }
        let mut partial = BTreeMap::new();
        partial.insert("model_list".to_owned(), ProfileCapabilityState::Supported);
        partial.insert(
            "account_read".to_owned(),
            ProfileCapabilityState::RecognizedUnsupported,
        );
        let snapshot = capability_snapshot(Some(&partial));
        assert_eq!(snapshot.len(), PROFILE_CAPABILITY_NAMES.len());
        assert_eq!(snapshot["model_list"], ProfileCapabilityState::Supported);
        assert_eq!(
            snapshot["account_read"],
            ProfileCapabilityState::RecognizedUnsupported
        );
        assert_eq!(
            snapshot["early_response_id"],
            ProfileCapabilityState::Unverified
        );
    }

    #[test]
    fn probe_server_never_claims_early_response_id_supported() {
        // early_response_id cannot be genuinely proven from this module's
        // edit scope (see the comment in probe_server), so the closed
        // baseline it starts from must never be silently overridden to
        // Supported for that name by the promotion loop.
        let baseline = capability_snapshot(None);
        assert_eq!(
            baseline["early_response_id"],
            ProfileCapabilityState::Unverified
        );
    }

    #[test]
    fn process_identity_matches_is_self_consistent_and_rejects_reuse() {
        let pid = std::process::id();
        let identity = DarwinSystem.live_process_identity(pid).unwrap();
        assert!(process_identity_matches(
            pid,
            identity.uid,
            identity.process_group_id,
            &identity.fingerprint
        ));
        assert!(!process_identity_matches(
            pid,
            identity.uid,
            identity.process_group_id,
            "not-the-recorded-fingerprint"
        ));
        assert!(!process_identity_matches(
            pid,
            identity.uid.wrapping_add(1),
            identity.process_group_id,
            &identity.fingerprint
        ));
    }

    #[test]
    fn process_identity_matches_is_false_for_a_dead_pid() {
        // A PID this high is never a live process in the test environment,
        // so this exercises the "no such process" path deterministically.
        assert!(!process_identity_matches(0x7fff_fffe, 0, 0, "anything"));
    }

    #[test]
    fn server_lifetime_absent_is_true_with_no_recorded_state() {
        let context = Context {
            workspace_id: "test".to_owned(),
            registry_path: std::env::temp_dir().join("unused.yaml"),
            application_support_root: std::env::temp_dir()
                .join(format!("dolgorae-lifetime-test-{}", Uuid::now_v7())),
        };
        assert!(server_lifetime_absent(&context, &"b".repeat(64)).unwrap());
    }

    #[test]
    fn repair_stale_migration_fence_handles_absent_blocked_lifetimes() {
        let context = Context {
            workspace_id: "test".to_owned(),
            registry_path: std::env::temp_dir().join("unused.yaml"),
            application_support_root: std::env::temp_dir()
                .join(format!("dolgorae-repair-test-{}", Uuid::now_v7())),
        };
        let home = context.application_support_root.join("homes").join("h");
        secure_dir(&home).unwrap();
        assert!(!repair_stale_migration_fence(&context, &home, &"c".repeat(64)).unwrap());

        let migration_path = home.join("migration.json");
        atomic_replace(
            &migration_path,
            serde_json::to_vec_pretty(&json!({
                "migration_id": Uuid::now_v7(),
                "phase": "committed",
                "old_server_key": "c".repeat(64),
                "new_server_key": "d".repeat(64),
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        assert!(!repair_stale_migration_fence(&context, &home, &"c".repeat(64)).unwrap());
        assert!(!repair_stale_migration_fence(&context, &home, &"e".repeat(64)).unwrap());

        atomic_replace(
            &migration_path,
            serde_json::to_vec_pretty(&json!({
                "migration_id": Uuid::now_v7(),
                "phase": "applying",
                "old_server_key": "c".repeat(64),
                "new_server_key": "d".repeat(64),
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        assert!(!repair_stale_migration_fence(&context, &home, &"c".repeat(64)).unwrap());
        let in_flight: Value = serde_json::from_slice(&fs::read(&migration_path).unwrap()).unwrap();
        assert_eq!(in_flight["phase"], "applying");

        atomic_replace(
            &migration_path,
            serde_json::to_vec_pretty(&json!({
                "migration_id": Uuid::now_v7(),
                "phase": "migration_blocked",
                "old_server_key": "c".repeat(64),
                "new_server_key": "d".repeat(64),
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        assert!(repair_stale_migration_fence(&context, &home, &"c".repeat(64)).unwrap());
        let repaired: Value = serde_json::from_slice(&fs::read(&migration_path).unwrap()).unwrap();
        assert_eq!(repaired["phase"], "rolled_back");
        fs::remove_dir_all(&context.application_support_root).unwrap();
    }

    #[test]
    fn profile_mismatch_and_profile_server_busy_match_the_checked_shapes() {
        let mismatch = profile_mismatch("demo", "field_name", json!("expected"), json!("actual"));
        assert_eq!(mismatch.code, "PROFILE_MISMATCH");
        assert!(!mismatch.retryable);
        assert_eq!(
            mismatch
                .details
                .as_object()
                .unwrap()
                .keys()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                &"profile".to_owned(),
                &"field".to_owned(),
                &"expected".to_owned(),
                &"actual".to_owned()
            ])
        );

        let busy = profile_server_busy("demo", &"a".repeat(64), "reason");
        assert_eq!(busy.code, "PROFILE_SERVER_BUSY");
        assert!(busy.retryable);
        assert_eq!(
            busy.details
                .as_object()
                .unwrap()
                .keys()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                &"profile".to_owned(),
                &"server_key".to_owned(),
                &"reason".to_owned()
            ])
        );

        let membership = profile_membership_incomplete("demo", &"a".repeat(64), "reason");
        assert_eq!(membership.code, "PROFILE_MEMBERSHIP_INCOMPLETE");
        assert!(!membership.retryable);

        let operator = operator_mismatch("profile.state.reset");
        assert_eq!(operator.code, "OPERATOR_MISMATCH");
        assert_eq!(
            operator
                .details
                .as_object()
                .unwrap()
                .keys()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([&"operation".to_owned()])
        );
    }

    /// A store rooted in a fresh private directory plus a carrier for the
    /// credential it just registered, so operator tests never touch the real
    /// user-private operator root.
    fn temporary_operator_store() -> (
        PathBuf,
        crate::controller::OperatorStore,
        crate::controller::CredentialCarrier,
    ) {
        let root =
            std::env::temp_dir().join(format!("dolgorae-profile-operator-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();
        let store = crate::controller::OperatorStore::new(root.join("operator"));
        let credential = root.join("operator-1.json");
        store.initialize(&credential).unwrap();
        let carrier = crate::controller::CredentialCarrier::open_path(&credential).unwrap();
        (root, store, carrier)
    }

    #[test]
    fn the_operator_hold_survives_until_the_next_lock_in_the_order_is_taken() {
        let (root, store, carrier) = temporary_operator_store();
        let mut operator = authorize_operator_in(&store, &carrier, "profile.server.stop").unwrap();
        assert!(store.try_lock_exclusive().unwrap().is_none());

        // Everything between authorization and the next lock — confirmation
        // checks, state reads, the fsynced fence a migration writes — happens
        // with the hold still live.
        let home_lock = lock_file(&root.join("home.lock")).unwrap();
        assert!(store.try_lock_exclusive().unwrap().is_none());

        operator.handoff(&home_lock);
        assert!(store.try_lock_exclusive().unwrap().is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rotation_waits_for_the_handoff_and_then_bars_the_old_generation_from_a_new_effect() {
        let (root, store, carrier) = temporary_operator_store();
        let mut operator = authorize_operator_in(&store, &carrier, "profile.server.stop").unwrap();

        // Contention is established before any rotator exists, so it can only
        // come from the hold this stop is carrying.
        assert!(store.try_lock_exclusive().unwrap().is_none());
        let home_lock = lock_file(&root.join("home.lock")).unwrap();
        assert!(store.try_lock_exclusive().unwrap().is_none());

        let rotated_credential = root.join("operator-2.json");
        let (started, running) = std::sync::mpsc::channel();
        let rotation_root = root.clone();
        let rotator = std::thread::spawn(move || {
            let store = crate::controller::OperatorStore::new(rotation_root.join("operator"));
            let carrier = crate::controller::CredentialCarrier::open_path(
                &rotation_root.join("operator-1.json"),
            )
            .unwrap();
            started.send(()).unwrap();
            store.rotate(&carrier, &rotation_root.join("operator-2.json"))
        });
        running.recv().unwrap();

        // The rotator is live and wants the same lock, but the stop has not
        // reached its handoff boundary yet. Not a timing guess: rotation
        // writes its first byte only after it takes the lock.
        assert!(!rotated_credential.exists());

        operator.handoff(&home_lock);
        let rotated = rotator.join().unwrap().unwrap();
        assert_eq!(rotated.credential.operator_generation, 2);

        // The effect that already handed off keeps running under the home
        // lock, but the rotated-away credential can no longer begin another.
        assert_eq!(
            authorize_operator_in(&store, &carrier, "profile.server.stop")
                .unwrap_err()
                .code,
            "OPERATOR_MISMATCH"
        );
        drop(home_lock);
        fs::remove_dir_all(root).unwrap();
    }

    /// The contract closes `OPERATION_TIMEOUT` to
    /// `{operation: replay|schema_generation|profile_doctor, budget_ms}` and
    /// pins its `retryable` to `true`. A socket bind is none of those three
    /// operations, so the timeout it raises has to be the retryable transport
    /// failure the contract does define for a connect that wrote nothing.
    #[test]
    fn a_socket_bind_timeout_is_a_contract_valid_retryable_transport_failure() {
        let root = std::env::temp_dir().join(format!("dolgorae-bind-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();
        // PID 1 always exists, so the wait can only end by expiring.
        let error = wait_for_socket(&root.join("never.sock"), 1).unwrap_err();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(error.code, "TRANSPORT_FAILURE");
        assert!(error.retryable);
        assert_eq!(error.details["stage"], "connect");
        assert_eq!(error.details["acceptance"], "not_written");
        assert_eq!(error.details["request_id"], Value::Null);
        assert_eq!(
            error
                .details
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["acceptance", "request_id", "stage"]
        );
    }

    /// A dead PID ends the same wait through the other branch, and it is the
    /// same contract-valid shape rather than a second vocabulary.
    #[test]
    fn a_server_that_exits_before_binding_reports_the_same_transport_shape() {
        let error = wait_for_socket(Path::new("/nonexistent/never.sock"), u32::MAX).unwrap_err();
        assert_eq!(error.code, "TRANSPORT_FAILURE");
        assert!(error.retryable);
        assert_eq!(error.details["stage"], "connect");
    }

    fn profile_with(environment: BTreeMap<String, String>) -> RuntimeProfile {
        RuntimeProfile {
            argv: vec!["/usr/bin/codex".to_owned()],
            codex_home: "/tmp".to_owned(),
            environment,
            native_subagents: NativeSubagents::Enabled,
        }
    }

    /// The five reserved names are account and platform facts. A caller that
    /// exports a different `HOME`, `USER`, `SHELL`, or `TMPDIR` must not be
    /// able to steer the launch contract through them.
    #[test]
    fn reserved_account_names_come_from_the_platform_and_not_from_the_caller() {
        let locale = "en_US.UTF-8".to_owned();
        let environment = BTreeMap::from([
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("LANG".to_owned(), locale.clone()),
            ("LC_ALL".to_owned(), locale),
        ]);
        let account = DarwinSystem.account_environment().unwrap();
        let prepared = prepared_environment(&profile_with(environment)).unwrap();

        assert_eq!(prepared["HOME"], account.home.to_str().unwrap());
        assert_eq!(prepared["USER"], account.user);
        assert_eq!(prepared["LOGNAME"], account.user);
        assert_eq!(prepared["SHELL"], account.shell.to_str().unwrap());
        assert_eq!(
            prepared["TMPDIR"],
            account.temporary_directory.to_str().unwrap()
        );
        // The account database is the authority, so nothing here can be the
        // inherited value unless the platform independently agrees with it.
        for name in ["HOME", "USER", "LOGNAME", "SHELL", "TMPDIR"] {
            let inherited = std::env::var(name).ok();
            assert!(
                inherited.is_none() || inherited.as_deref() != Some("/dolgorae-not-an-account"),
                "{name} must not be sourced from the caller environment"
            );
        }
        assert!(Path::new(&prepared["HOME"]).is_absolute());
        assert!(Path::new(&prepared["TMPDIR"]).is_absolute());
    }

    /// A Codex app-server writes ordinary diagnostic text. Treating "not JSON"
    /// as "unredactable" turned the whole private log into drop markers.
    #[test]
    fn the_log_drainer_keeps_redacted_plain_text_and_drops_only_what_it_cannot_redact() {
        assert_eq!(
            redacted_log_body(b"INFO listening on unix:///tmp/p/abc.sock\n"),
            "INFO listening on unix:///tmp/p/abc.sock"
        );
        assert_eq!(
            redacted_log_body(b"2026-08-22T00:00:00Z WARN retrying in 5s\r\n"),
            "2026-08-22T00:00:00Z WARN retrying in 5s"
        );
        // A secret-bearing name redacts to end of line: a value is not
        // reliably one whitespace-delimited token.
        assert_eq!(
            redacted_log_body(b"header Authorization: Bearer sk-live-1234567890\n"),
            "header Authorization: [DOLGORAE_REDACTED]"
        );
        assert_eq!(
            redacted_log_body(b"api_key=sk-live-1 other=kept\n"),
            "api_key=[DOLGORAE_REDACTED]"
        );
        // A name that merely mentions a benign word is left alone.
        assert_eq!(
            redacted_log_body(b"keyboard=attached\n"),
            "keyboard=attached"
        );
        // JSON still goes through the canonical redacting representation.
        assert_eq!(
            redacted_log_body(b"{\"b\":1,\"api_key\":\"sk-live\"}\n"),
            "{\"api_key\":{\"$dolgorae_redacted\":{\"original_type\":\"string\",\"reason\":\"secret_key\"}},\"b\":1}"
        );
        // The drop marker is reserved for a line redaction genuinely failed
        // on: invalid UTF-8, and JSON the canonical redactor refuses.
        assert_eq!(redacted_log_body(&[b'{', 0xff, b'}']), DROPPED_LOG_MARKER);
        assert_eq!(
            redacted_log_body(b"{\"a\":1,\"a\":2}\n"),
            DROPPED_LOG_MARKER
        );
    }

    fn membership_record(revision: u64, kind: &str, run_id: Option<&str>) -> MembershipRecord {
        MembershipRecord {
            schema_version: 1,
            revision,
            kind: kind.to_owned(),
            workspace_id: "workspace".to_owned(),
            run_id: run_id.map(str::to_owned),
            previous_sha256: "0".repeat(64),
            record_sha256: "0".repeat(64),
        }
    }

    /// Every membership gate reads the derived index. Before a Run could be
    /// registered, that index was always empty and every gate was vacuous, so
    /// the registration kinds are what make the gates mean anything.
    #[test]
    fn run_registration_and_release_move_a_run_in_and_out_of_the_live_member_set() {
        let root = std::env::temp_dir().join(format!("dolgorae-members-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();

        let registered = derive_membership_index(
            &root,
            &[
                membership_record(1, "server_started", None),
                membership_record(2, "run_registered", Some("run-a")),
                membership_record(3, "run_registered", Some("run-b")),
            ],
        )
        .unwrap();
        assert_eq!(
            run_members(&registered)
                .iter()
                .map(|member| member.run_id.clone())
                .collect::<Vec<_>>(),
            ["run-a", "run-b"]
        );
        assert_eq!(live_member_evidence(&registered).len(), 2);

        let released = derive_membership_index(
            &root,
            &[
                membership_record(1, "server_started", None),
                membership_record(2, "run_registered", Some("run-a")),
                membership_record(3, "run_registered", Some("run-b")),
                membership_record(4, "run_closed", Some("run-a")),
                membership_record(5, "run_migrated", Some("run-b")),
            ],
        )
        .unwrap();
        assert!(run_members(&released).is_empty());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_release_kind_names_the_journal_vocabulary_the_index_already_understood() {
        assert_eq!(RunMemberRelease::Closed.kind(), "run_closed");
        assert_eq!(RunMemberRelease::Migrated.kind(), "run_migrated");
    }

    /// The membership run identity is a journal key, so a value that could
    /// forge the `workspace:run` identity or escape the profile directory is
    /// an invalid argument rather than a silently accepted record.
    #[test]
    fn a_membership_run_identity_may_not_forge_its_own_key() {
        let state = stopped_state();
        for candidate in ["", "workspace:run", "../run"] {
            let error = record_membership(None, &state, candidate, "run_registered").unwrap_err();
            assert_eq!(error.code, "INVALID_ARGUMENT", "accepted {candidate:?}");
        }
    }

    fn stopped_state() -> ServerState {
        ServerState {
            schema_version: 1,
            server_key: "a".repeat(64),
            lifecycle: "ready".to_owned(),
            server_epoch: 1,
            epoch_id: Uuid::now_v7(),
            pid: 1,
            pgid: 1,
            uid: DarwinSystem.current_uid(),
            process_fingerprint: "fingerprint".to_owned(),
            drainer_pid: 1,
            drainer_pgid: 1,
            drainer_uid: DarwinSystem.current_uid(),
            drainer_fingerprint: "fingerprint".to_owned(),
            socket_path: "/tmp/never.sock".to_owned(),
            socket_device: 0,
            socket_inode: 0,
            membership_revision: 1,
            default_model: "model".to_owned(),
            models: vec!["model".to_owned()],
            capabilities: capability_snapshot(None),
            snapshot: stopped_snapshot(),
        }
    }

    fn stopped_snapshot() -> ProfileSnapshot {
        ProfileSnapshot {
            schema_version: 1,
            profile_name: "default".to_owned(),
            canonical_codex_home: "/tmp".to_owned(),
            normalized_argv: vec!["/usr/bin/codex".to_owned()],
            launch_cwd_policy: "profile_state_directory_v1".to_owned(),
            derived_launch_cwd: "/tmp".to_owned(),
            sanitized_environment: BTreeMap::new(),
            enabled_features: Vec::new(),
            disabled_features: Vec::new(),
            process_static_configuration: BTreeMap::new(),
            initial_configuration_observation: BTreeMap::new(),
            executable_identity: ExecutableIdentity {
                resolved_path: "/usr/bin/codex".to_owned(),
                device: 0,
                inode: 0,
                sha256: "0".repeat(64),
            },
            codex_version: SUPPORTED_CODEX_VERSION.to_owned(),
            schema_bundle_sha256: "0".repeat(64),
            compatibility_manifest_sha256: "0".repeat(64),
            launch_contract_sha256: "0".repeat(64),
            compatibility_verdict: CompatibilityVerdict::Tested,
            server_key: "a".repeat(64),
        }
    }

    #[test]
    fn migration_and_stop_reservations_are_mutually_exclusive_in_both_orders() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-stop-migration-order-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();
        let migration_path = root.join("migration.json");
        let migration_id = Uuid::now_v7();
        atomic_replace(
            &migration_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "migration_id": migration_id,
                "old_server_key": "a".repeat(64),
                "new_server_key": "b".repeat(64),
                "phase": "prepared",
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();

        // Migration-first: an unrelated operator stop cannot pass the
        // durable fence, while the stop owned by that migration can.
        let unrelated =
            verify_migration_fence(&migration_path, None, "default", &"a".repeat(64)).unwrap_err();
        assert_eq!(unrelated.code, "PROFILE_SERVER_BUSY");
        verify_migration_fence(
            &migration_path,
            Some(migration_id),
            "default",
            &"a".repeat(64),
        )
        .unwrap();

        atomic_replace(
            &migration_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "migration_id": "not-a-uuid",
                "old_server_key": "a".repeat(64),
                "new_server_key": "b".repeat(64),
                "phase": "applying",
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        let malformed =
            verify_migration_fence(&migration_path, None, "default", &"a".repeat(64)).unwrap_err();
        assert_eq!(malformed.code, "TRANSPORT_FAILURE");

        atomic_replace(
            &migration_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "migration_id": migration_id,
                "old_server_key": "a".repeat(64),
                "new_server_key": "b".repeat(64),
                "phase": "preprared",
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();
        let malformed_phase =
            verify_migration_fence(&migration_path, None, "default", &"a".repeat(64)).unwrap_err();
        assert_eq!(malformed_phase.code, "TRANSPORT_FAILURE");

        for phase in ["committed", "rolled_back"] {
            for invalid_id in [Value::Null, Value::String("not-a-uuid".to_owned())] {
                atomic_replace(
                    &migration_path,
                    serde_json::to_vec_pretty(&json!({
                        "schema_version": 1,
                        "migration_id": invalid_id,
                        "old_server_key": "a".repeat(64),
                        "new_server_key": "b".repeat(64),
                        "phase": phase,
                    }))
                    .unwrap()
                    .as_slice(),
                )
                .unwrap();
                let invalid_terminal =
                    verify_migration_fence(&migration_path, None, "default", &"a".repeat(64))
                        .unwrap_err();
                assert_eq!(invalid_terminal.code, "TRANSPORT_FAILURE");
            }
        }

        // Stop-first: a migration-owned stop cannot replace the token of a
        // stop already in its lock-free APPLY phase.
        let snapshot = stopped_snapshot();
        let state = stopped_state();
        let stopping = HomeActive {
            schema_version: 1,
            canonical_codex_home: snapshot.canonical_codex_home.clone(),
            server_key: snapshot.server_key.clone(),
            server_epoch: state.server_epoch,
            lifecycle: "stopping".to_owned(),
            pid: Some(state.pid),
            transition_token: Some(Uuid::now_v7()),
        };
        let occupied = verify_stop_home_active(&snapshot, &state, Some(&stopping)).unwrap_err();
        assert_eq!(occupied.code, "PROFILE_SERVER_BUSY");

        let ready = HomeActive {
            lifecycle: "ready".to_owned(),
            transition_token: None,
            ..stopping
        };
        verify_stop_home_active(&snapshot, &state, Some(&ready)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_persistence_retries_are_bounded_and_report_exhaustion() {
        let attempts = std::cell::Cell::new(0);
        persist_migration_with(
            Path::new("unused"),
            &json!({"phase": "committed"}),
            3,
            |_, _| {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 3 {
                    Err(transport("injected migration write failure"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!(attempts.get(), 3);

        attempts.set(0);
        let exhausted = persist_migration_with(
            Path::new("unused"),
            &json!({"phase": "migration_blocked"}),
            3,
            |_, _| {
                attempts.set(attempts.get() + 1);
                Err(transport("injected migration write failure"))
            },
        )
        .unwrap_err();
        assert_eq!(attempts.get(), 3);
        assert_eq!(exhausted.code, "TRANSPORT_FAILURE");
    }

    #[test]
    fn migration_confirmation_keys_are_canonical_before_path_construction() {
        require_canonical_server_key("--confirm-old-server-key", &"a0".repeat(32)).unwrap();
        for invalid in [
            "../outside".to_owned(),
            "/tmp/outside".to_owned(),
            "A".repeat(64),
            "g".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
        ] {
            let error =
                require_canonical_server_key("--confirm-old-server-key", &invalid).unwrap_err();
            assert_eq!(error.code, "INVALID_ARGUMENT", "accepted {invalid:?}");
            assert_eq!(error.details["argument"], "--confirm-old-server-key");
        }
    }

    #[test]
    fn durable_migration_keys_are_canonical_before_path_construction() {
        let root = std::env::temp_dir().join(format!(
            "dolgorae-invalid-durable-migration-key-{}",
            Uuid::now_v7()
        ));
        secure_dir(&root).unwrap();
        let migration_path = root.join("migration.json");
        atomic_replace(
            &migration_path,
            &serde_json::to_vec_pretty(&json!({
                "migration_id": Uuid::now_v7(),
                "phase": "committed",
                "old_server_key": "../outside",
                "new_server_key": "b".repeat(64),
            }))
            .unwrap(),
        )
        .unwrap();
        let error = read_migration_record(&migration_path).unwrap_err();
        assert_eq!(error.code, "TRANSPORT_FAILURE");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_rollover_attaches_to_the_requested_ready_generation() {
        let support =
            std::env::temp_dir().join(format!("dolgorae-duplicate-rollover-{}", Uuid::now_v7()));
        let old_key = "a".repeat(64);
        let new_key = "b".repeat(64);
        let context = Context {
            workspace_id: "test".to_owned(),
            registry_path: support.join("unused.yaml"),
            application_support_root: support.clone(),
        };
        let mut snapshot = stopped_snapshot();
        snapshot.server_key = new_key.clone();
        let home = home_root(&context, &snapshot.canonical_codex_home).unwrap();
        let old_root = support.join("profiles").join(&old_key);
        let new_root = support.join("profiles").join(&new_key);
        secure_dir(&old_root).unwrap();
        secure_dir(&new_root).unwrap();
        let socket = PathBuf::from(format!("/tmp/ddr-{}.sock", Uuid::now_v7()));
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let socket_metadata = fs::symlink_metadata(&socket).unwrap();
        let identity = DarwinSystem
            .live_process_identity(std::process::id())
            .unwrap();
        let mut state = stopped_state();
        state.server_key = new_key.clone();
        state.snapshot = snapshot.clone();
        state.pid = std::process::id();
        state.pgid = identity.process_group_id;
        state.uid = identity.uid;
        state.process_fingerprint = identity.fingerprint;
        state.socket_path = socket.to_str().unwrap().to_owned();
        state.socket_device = socket_metadata.dev();
        state.socket_inode = socket_metadata.ino();
        atomic_replace(
            &new_root.join("state.json"),
            &serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
        write_home_active(
            &home.join("active.json"),
            &HomeActive {
                schema_version: 1,
                canonical_codex_home: snapshot.canonical_codex_home.clone(),
                server_key: new_key,
                server_epoch: state.server_epoch,
                lifecycle: "ready".to_owned(),
                pid: Some(state.pid),
                transition_token: None,
            },
        )
        .unwrap();

        let attached = automatic_quiescent_migration(&context, &snapshot, &old_key).unwrap();
        assert_eq!(attached, state);

        drop(listener);
        fs::remove_file(socket).unwrap();
        fs::remove_dir_all(support).unwrap();
    }

    #[test]
    fn operator_reconciliation_commits_a_blocked_ready_replacement() {
        let support = std::env::temp_dir().join(format!(
            "dolgorae-blocked-migration-recovery-{}",
            Uuid::now_v7()
        ));
        let new_key = "b".repeat(64);
        let old_key = "a".repeat(64);
        let new_root = support.join("profiles").join(&new_key);
        let home = support.join("homes").join("home");
        secure_dir(&new_root).unwrap();
        secure_dir(&home).unwrap();
        let socket = PathBuf::from(format!("/tmp/dmr-{}.sock", Uuid::now_v7()));
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let socket_metadata = fs::symlink_metadata(&socket).unwrap();
        let identity = DarwinSystem
            .live_process_identity(std::process::id())
            .unwrap();

        let mut snapshot = stopped_snapshot();
        snapshot.server_key = new_key.clone();
        let mut state = stopped_state();
        state.server_key = new_key.clone();
        state.snapshot = snapshot.clone();
        state.pid = std::process::id();
        state.pgid = identity.process_group_id;
        state.uid = identity.uid;
        state.process_fingerprint = identity.fingerprint;
        state.socket_path = socket.to_str().unwrap().to_owned();
        state.socket_device = socket_metadata.dev();
        state.socket_inode = socket_metadata.ino();
        atomic_replace(
            &new_root.join("state.json"),
            &serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
        write_home_active(
            &home.join("active.json"),
            &HomeActive {
                schema_version: 1,
                canonical_codex_home: snapshot.canonical_codex_home.clone(),
                server_key: new_key.clone(),
                server_epoch: state.server_epoch,
                lifecycle: "ready".to_owned(),
                pid: Some(state.pid),
                transition_token: None,
            },
        )
        .unwrap();
        let migration_id = Uuid::now_v7();
        let migration_path = home.join("migration.json");
        atomic_replace(
            &migration_path,
            &serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "migration_id": migration_id,
                "old_server_key": old_key,
                "new_server_key": new_key,
                "phase": "migration_blocked",
            }))
            .unwrap(),
        )
        .unwrap();
        let context = Context {
            workspace_id: "test".to_owned(),
            registry_path: support.join("unused.yaml"),
            application_support_root: support.clone(),
        };

        let recovered = reconcile_blocked_migration(
            &context,
            &snapshot,
            &old_key,
            &new_key,
            &home,
            &migration_path,
        )
        .unwrap()
        .unwrap();
        assert_eq!(recovered.0, migration_id);
        assert_eq!(recovered.1.server_key, new_key);
        let migration: Value = serde_json::from_slice(&fs::read(&migration_path).unwrap()).unwrap();
        assert_eq!(migration["phase"], "committed");

        drop(listener);
        fs::remove_file(socket).unwrap();
        fs::remove_dir_all(support).unwrap();
    }

    /// Attaching is how every Run reaches the singleton, so it asks the same
    /// socket-identity question a stop asks: the recorded device and inode,
    /// not just the pathname, which an unrelated socket can reuse between two
    /// lifetimes.
    #[test]
    fn attaching_refuses_a_socket_that_is_no_longer_the_recorded_device_and_inode() {
        // Directly under /tmp: a Unix socket pathname has to fit SUN_LEN,
        // and the per-user temporary directory alone nearly exhausts it.
        let root = PathBuf::from(format!("/tmp/dolgorae-attach-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();
        let socket = root.join("live.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let metadata = fs::symlink_metadata(&socket).unwrap();

        let mut state = stopped_state();
        state.socket_path = socket.to_str().unwrap().to_owned();
        state.socket_device = metadata.dev();
        state.socket_inode = metadata.ino();
        verify_recorded_socket("default", &state).expect("the recorded socket verifies");

        // The same pathname, a different socket: a new lifetime bound over
        // the old one, which a pathname-only check cannot see.
        drop(listener);
        fs::remove_file(&socket).unwrap();
        let replacement = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let error = verify_recorded_socket("default", &state).unwrap_err();
        drop(replacement);

        assert_eq!(error.code, "PROFILE_MISMATCH");
        assert_eq!(error.details["field"], "socket_identity");
        assert_eq!(
            error.details["expected"]["socket_inode"],
            json!(state.socket_inode)
        );

        // An absent socket is the same mismatch, never a silent success.
        fs::remove_file(&socket).unwrap();
        assert_eq!(
            verify_recorded_socket("default", &state).unwrap_err().code,
            "PROFILE_MISMATCH"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    fn home_active(epoch: u64, lifecycle: &str, token: Option<Uuid>) -> HomeActive {
        HomeActive {
            schema_version: 1,
            canonical_codex_home: "/tmp".to_owned(),
            server_key: "a".repeat(64),
            server_epoch: epoch,
            lifecycle: lifecycle.to_owned(),
            pid: None,
            transition_token: token,
        }
    }

    /// COMMIT runs after APPLY released both locks, so its only defence
    /// against a lifetime that changed underneath it is the reservation the
    /// account home still carries.
    #[test]
    fn a_commit_requires_the_exact_token_epoch_and_phase_prepare_stamped() {
        let root = std::env::temp_dir().join(format!("dolgorae-token-{}", Uuid::now_v7()));
        secure_dir(&root).unwrap();
        let active_path = root.join("active.json");
        let snapshot = stopped_snapshot();
        let token = Uuid::now_v7();

        // No record at all: nothing to commit into.
        let error = require_transition(&active_path, &snapshot, 7, token, "starting").unwrap_err();
        assert_eq!(error.details["field"], "home_transition_token");
        assert_eq!(error.details["actual"], Value::Null);

        write_home_active(&active_path, &home_active(7, "starting", Some(token))).unwrap();
        require_transition(&active_path, &snapshot, 7, token, "starting").unwrap();

        // A record written before the token existed can never satisfy a
        // commit that is looking for one.
        write_home_active(&active_path, &home_active(7, "starting", None)).unwrap();
        assert!(require_transition(&active_path, &snapshot, 7, token, "starting").is_err());

        // Another transition took the home: different token, then different
        // epoch, then a phase this commit never reserved.
        write_home_active(
            &active_path,
            &home_active(7, "starting", Some(Uuid::now_v7())),
        )
        .unwrap();
        assert!(require_transition(&active_path, &snapshot, 7, token, "starting").is_err());
        write_home_active(&active_path, &home_active(8, "starting", Some(token))).unwrap();
        assert!(require_transition(&active_path, &snapshot, 7, token, "starting").is_err());
        write_home_active(&active_path, &home_active(7, "ready", Some(token))).unwrap();
        assert!(require_transition(&active_path, &snapshot, 7, token, "starting").is_err());

        fs::remove_dir_all(&root).unwrap();
    }

    /// A record still in `starting` or `stopping` is another call inside its
    /// lock-free APPLY window. Spawning past it would be the double launch the
    /// singleton exists to prevent, so it is a retryable busy, not a mismatch
    /// a caller would read as a permanent contract failure.
    #[test]
    fn a_transition_already_in_flight_is_a_retryable_busy_rather_than_a_double_launch() {
        let snapshot = stopped_snapshot();
        let mut state = stopped_state();
        state.server_epoch = 3;
        for lifecycle in ["starting", "stopping"] {
            let active = HomeActive {
                server_epoch: 3,
                pid: Some(state.pid),
                ..home_active(3, lifecycle, Some(Uuid::now_v7()))
            };
            let error = attach_running(&snapshot, &state, Some(&active)).unwrap_err();
            assert_eq!(error.code, "PROFILE_SERVER_BUSY", "for {lifecycle}");
            assert!(error.retryable);
        }
    }
}
