//! Isolated native fake operations for black-box tests. Never linked into the product.
use crate::machine::{FailureEnvelope, MachineError, SuccessEnvelope};
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    root: PathBuf,
    arguments: Vec<String>,
}

fn validate_root(
    root: &Path,
    arguments: &[String],
) -> Result<crate::paths::DolgoraeHome, MachineError> {
    let canonical = root.canonicalize().map_err(|_| invalid())?;
    let temporary = std::env::temp_dir().canonicalize().map_err(|_| invalid())?;
    let permitted = canonical.starts_with(&temporary)
        || canonical.starts_with("/private/tmp")
        || canonical.starts_with("/Volumes/RootKernel/tmp");
    let metadata = fs::symlink_metadata(root).map_err(|_| invalid())?;
    if canonical != root
        || !permitted
        || !metadata.is_dir()
        || metadata.uid() != crate::darwin::DarwinSystem.current_uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(invalid());
    }
    let home = crate::paths::DolgoraeHome::system()?;
    if !home.root().starts_with(root) || home.root() == root {
        return Err(invalid());
    }
    crate::global_profile::require_generation(&home)?;
    let selected_profile = arguments
        .windows(2)
        .find(|pair| pair[0] == "--profile")
        .map(|pair| pair[1].as_str());
    for (name, definition) in crate::global_profile::GlobalProfileStore::new(&home)
        .load()?
        .profiles
        .iter()
    {
        // Drift tests deliberately damage earlier Profiles. Only the selected
        // Profile can launch for a one-shot request; validate that one without
        // changing another historical record's failure semantics.
        if selected_profile.is_some_and(|selected| selected != name) {
            continue;
        }
        let executable = definition.argv.first().ok_or_else(invalid)?;
        let executable = Path::new(executable)
            .canonicalize()
            .map_err(|_| invalid())?;
        let codex_home = Path::new(&definition.codex_home)
            .canonicalize()
            .map_err(|_| invalid())?;
        if !executable.starts_with(root) || !codex_home.starts_with(root) {
            return Err(invalid());
        }
        let metadata = fs::metadata(&executable).map_err(|_| invalid())?;
        if metadata.len() > 32 * 1024 * 1024 {
            return Err(invalid());
        }
        let bytes = fs::read(&executable).map_err(|_| invalid())?;
        let driver = executable.with_file_name(format!(
            "{}-driver.py",
            executable
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(invalid)?
        ));
        let driver_metadata = fs::symlink_metadata(&driver).map_err(|_| invalid())?;
        if !driver_metadata.is_file() || driver_metadata.len() > 1024 * 1024 {
            return Err(invalid());
        }
        let driver_bytes = fs::read(&driver).map_err(|_| invalid())?;
        let driver_path = driver.to_str().ok_or_else(invalid)?.as_bytes();
        let marker = b"server_module.FakeAppServer(";
        if !bytes
            .windows(driver_path.len())
            .any(|window| window == driver_path)
            || !driver_bytes
                .windows(marker.len())
                .any(|window| window == marker)
            || codex_home.join("auth.json").exists()
        {
            return Err(invalid());
        }
    }
    Ok(home)
}

fn invalid() -> MachineError {
    MachineError::invalid_argument(
        "fixture",
        "historical fixture requires private isolated native fake state",
    )
}

fn profile_name(args: &[OsString]) -> Result<String, MachineError> {
    crate::cli::option_path(args, "--profile")
        .map_err(|_| invalid())?
        .and_then(|value| value.into_os_string().into_string().ok())
        .ok_or_else(invalid)
}

fn concurrent_review_rollover(
    home: &crate::paths::DolgoraeHome,
    args: &[OsString],
) -> Result<Value, MachineError> {
    let workspace = crate::cli::option_path(args, "--workspace")
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    let view = crate::workspace::WorkspaceService::system()?.discover(Some(&workspace))?;
    let reference = args
        .windows(2)
        .find(|pair| pair[0] == "--request-ref")
        .and_then(|pair| pair[1].to_str())
        .ok_or_else(invalid)?;
    let owner = crate::profile::ReviewServerOwner {
        workspace_id: view.workspace_id,
        request_ref: uuid::Uuid::parse_str(reference).map_err(|_| invalid())?,
    };
    let binding =
        crate::global_runtime::ResolvedGlobalProfile::resolve(home, &profile_name(args)?)?
            .prepare_qualified(home)?;
    binding.record_qualified(home)?;
    let state = crate::profile::review_rollover_fixture(home, &binding, &owner, |old_key| {
        println!(
            "{}",
            json!({"phase": "rollover_prepared", "incumbent_key": old_key, "requested_key": binding.server_key}),
        );
        std::io::stdout().flush().map_err(|_| invalid())?;
        let mut resume = [0];
        std::io::stdin()
            .read_exact(&mut resume)
            .map_err(|_| invalid())?;
        if resume[0] != b'1' {
            return Err(invalid());
        }
        Ok(())
    })?;
    let retirement = crate::profile::retire_review_server(&binding, &owner)?;
    Ok(json!({"state": state, "retirement": retirement}))
}

fn start_former_manifest(
    home: &crate::paths::DolgoraeHome,
    args: &[OsString],
) -> Result<Value, MachineError> {
    use crate::jcs::{canonicalize, parse, sha256_hex};

    let resolved =
        crate::global_runtime::ResolvedGlobalProfile::resolve(home, &profile_name(args)?)?;
    let mut snapshot = resolved.clone().prepare_qualified(home)?.launch_snapshot;
    snapshot.compatibility_manifest_sha256 = sha256_hex(b"former offline fixture manifest");
    let contract = json!({
        "schema_version": 1,
        "canonical_codex_home": snapshot.canonical_codex_home,
        "normalized_argv": snapshot.normalized_argv,
        "launch_cwd_policy": snapshot.launch_cwd_policy,
        "launch_mode": "app_server_unix_socket_v1",
        "sanitized_environment": snapshot.sanitized_environment,
        "executable_identity": snapshot.executable_identity,
        "process_static_configuration": snapshot.process_static_configuration,
        "codex_version": snapshot.codex_version,
        "app_server_schema_sha256": snapshot.schema_bundle_sha256,
        "compatibility_manifest_sha256": snapshot.compatibility_manifest_sha256,
        "enabled_features": snapshot.enabled_features,
        "disabled_features": snapshot.disabled_features,
    });
    let canonical = canonicalize(&parse(&contract.to_string()).map_err(|_| invalid())?)
        .map_err(|_| invalid())?;
    snapshot.launch_contract_sha256 = sha256_hex(&canonical);
    let mut identity = b"dolgorae-profile-server-key-v1\0".to_vec();
    identity.extend_from_slice(&canonical);
    snapshot.server_key = sha256_hex(&identity);
    snapshot.derived_launch_cwd = home
        .root()
        .join("profiles")
        .join(&snapshot.server_key)
        .to_str()
        .ok_or_else(invalid)?
        .to_owned();
    let binding = resolved.bind_qualified(snapshot)?;
    binding.record_qualified(home)?;
    crate::profile::ensure_qualified_global_server(&binding).map(|state| json!(state))
}

fn start_specialist(
    home: &crate::paths::DolgoraeHome,
    args: &[OsString],
    kind: crate::domain::AggregateKind,
) -> Result<Value, MachineError> {
    use crate::domain::{AggregateKind, Assurance, ExecutionLane, Purpose, PurposeKind};
    use crate::run::{AggregateBinding, AggregateMemberKind, CallerAgentConfigurationSnapshot};
    use crate::semantic::{BrokeredSpecialistStartContext, ExternalSpecialistStartContext};
    use uuid::Uuid;

    let profile = profile_name(args)?;
    let resolved = crate::global_runtime::ResolvedGlobalProfile::resolve(home, &profile)?;
    let binding = match kind {
        AggregateKind::ExternalSpecialistEngagement => resolved.prepare_qualified(home)?,
        AggregateKind::OrchestratedSession => resolved.prepare_specialist(home, true)?,
    };
    binding.record_qualified(home)?;
    let configuration = CallerAgentConfigurationSnapshot {
        schema_version: 3,
        selected_profile: profile,
        global_profile_binding_sha256: binding.digest()?,
        model: "fixture-missing".to_owned(),
        default_effort: "medium".to_owned(),
        purpose: Purpose {
            kind: PurposeKind::Review,
            external_label: None,
        },
        required_capabilities: Vec::new(),
        role_reference: Some("reviewer".to_owned()),
        instructions: crate::instructions::InstructionSourceStore::new(home).capture("reviewer")?,
        execution_lane: ExecutionLane::Dedicated,
        required_assurance: Assurance::BestEffortPersonalAlpha,
        native_subagent_policy: "enabled".to_owned(),
    };
    let role_snapshot_sha256 = configuration.instructions.role.sha256.clone();
    let configuration = configuration.try_into()?;
    let run_id = args
        .windows(2)
        .find(|pair| pair[0] == "--fixture-start-ref")
        .map(|pair| Uuid::parse_str(pair[1].to_str().ok_or_else(invalid)?).map_err(|_| invalid()))
        .transpose()?
        .unwrap_or_else(Uuid::now_v7);
    let aggregate = AggregateBinding {
        aggregate_kind: kind,
        aggregate_id: Uuid::from_u128(run_id.as_u128() ^ 1),
        operation_id: Uuid::from_u128(run_id.as_u128() ^ 2),
        member_kind: AggregateMemberKind::Specialist,
        policy_sha256: None,
        role_reference: Some("reviewer".to_owned()),
        role_snapshot_sha256: Some(role_snapshot_sha256),
        agent_configuration_sha256: Some(
            crate::run::agent_configuration_digest(&configuration).map_err(|_| invalid())?,
        ),
    };
    match kind {
        AggregateKind::ExternalSpecialistEngagement => {
            crate::semantic::start_external_specialist_run(
                args,
                ExternalSpecialistStartContext {
                    reserved_run_id: run_id,
                    aggregate_binding: &aggregate,
                    agent_configuration: &configuration,
                    launch_cwd: None,
                    sandbox: "read-only",
                    global_profile_binding: &binding,
                },
            )
        }
        AggregateKind::OrchestratedSession => crate::semantic::start_brokered_specialist_run(
            args,
            BrokeredSpecialistStartContext {
                reserved_run_id: run_id,
                aggregate_binding: &aggregate,
                agent_configuration: &configuration,
                launch_cwd: None,
                sandbox: "read-only",
                global_profile_binding: &binding,
            },
        ),
    }
}

fn prepare(
    home: &crate::paths::DolgoraeHome,
    arguments: &[String],
) -> Result<(String, Value), MachineError> {
    let args = arguments
        .get(2..)
        .unwrap_or(&[])
        .iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    match arguments.first().map(String::as_str) {
        Some("review-target") if arguments.get(1).is_some_and(|arg| arg == "capture") => {
            crate::review_target::execute_with_source_contract(
                crate::review_target::Operation::Capture,
                &args,
                crate::workspace::SourceIsolationContract::LegacyBeforeCutover,
            )
            .map(|value| ("review-target.capture".to_owned(), value))
        }
        Some("specialist") if arguments.get(1).is_some_and(|arg| arg == "review") => {
            crate::review::execute_cli_with_source_contract(
                &args,
                crate::workspace::SourceIsolationContract::LegacyBeforeCutover,
            )
            .map(|value| ("specialist.review".to_owned(), value))
        }
        Some("specialist") if arguments.get(1).is_some_and(|arg| arg == "review-recover") => {
            // A live fixture Worker accepts mutations only from its own build.
            // Recovery uses the unchanged product service and accepted records.
            crate::review_recovery::execute_cli("specialist.review-recover", &args)
                .map(|value| ("specialist.review-recover".to_owned(), value))
        }
        Some("specialist") if arguments.get(1).is_some_and(|arg| arg == "policy") => {
            use crate::specialist_policy::PolicyOperation;
            let (operation, name) = match arguments.get(2).map(String::as_str) {
                Some("validate") => (PolicyOperation::Validate, "specialist.policy.validate"),
                Some("add") => (PolicyOperation::Add, "specialist.policy.add"),
                _ => return Err(invalid()),
            };
            crate::specialist_policy::execute(operation, &args[1..])
                .map(|value| (name.to_owned(), value))
        }
        Some("engagement") if arguments.get(1).is_some_and(|arg| arg == "call") => {
            crate::external_engagement::execute_cli_with_source_contract(
                &args,
                crate::workspace::SourceIsolationContract::LegacyBeforeCutover,
            )
            .map(|value| ("engagement.call".to_owned(), value))
        }
        Some("typed-start") => {
            let carrier = arguments.get(1).ok_or_else(invalid)?;
            let kind = match carrier.as_str() {
                "external" => crate::domain::AggregateKind::ExternalSpecialistEngagement,
                "brokered" => crate::domain::AggregateKind::OrchestratedSession,
                _ => return Err(invalid()),
            };
            start_specialist(home, &args, kind)
                .map(|value| (format!("typed-start.{carrier}"), value))
        }
        Some("rollover-fixture") => {
            let action = arguments.get(1).ok_or_else(invalid)?;
            let value = match action.as_str() {
                "former-manifest" => start_former_manifest(home, &args)?,
                "concurrent-review" => concurrent_review_rollover(home, &args)?,
                "retained-qualified" | "retained-successor" | "retained-legacy" => {
                    let resolved = crate::global_runtime::ResolvedGlobalProfile::resolve(
                        home,
                        &profile_name(&args)?,
                    )?;
                    let binding = match action.as_str() {
                        "retained-qualified" => resolved.prepare_qualified(home)?,
                        "retained-successor" => resolved.prepare_specialist(home, true)?,
                        _ => resolved.prepare(home)?,
                    };
                    if action != "retained-legacy" {
                        binding.record_qualified(home)?;
                    }
                    json!(crate::profile::ensure_retained_global_server(&binding)?)
                }
                _ => return Err(invalid()),
            };
            Ok((format!("rollover-fixture.{action}"), value))
        }
        _ => Err(invalid()),
    }
}

pub(crate) fn run_fixture() -> std::process::ExitCode {
    let request = PathBuf::from(std::env::var_os("DOLGORAE_ACCEPTED_FIXTURE_REQUEST").unwrap());
    let metadata = fs::symlink_metadata(&request).unwrap();
    assert!(metadata.is_file() && metadata.mode() & 0o077 == 0);
    assert_eq!(metadata.uid(), crate::darwin::DarwinSystem.current_uid());
    let bytes = fs::read(&request).unwrap();
    assert!(bytes.len() <= 1_048_576);
    let input: Input = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(request.parent(), Some(input.root.as_path()));
    let result = validate_root(&input.root, &input.arguments)
        .and_then(|home| prepare(&home, &input.arguments));
    let command = match input.arguments.get(0..2) {
        Some([group, leaf]) => format!("{group}.{leaf}"),
        _ => "unknown".to_owned(),
    };
    let (status, value) = match result {
        Ok((command, value)) => (0, json!(SuccessEnvelope::new(command, value))),
        Err(error) => (
            error.exit_status(),
            json!(FailureEnvelope::new(command, error)),
        ),
    };
    println!("{value}");
    std::process::ExitCode::from(status)
}
