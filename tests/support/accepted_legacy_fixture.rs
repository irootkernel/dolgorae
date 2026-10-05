//! Synthetic historical records for black-box recovery tests. Never linked into the product.
use crate::machine::{FailureEnvelope, MachineError, SuccessEnvelope};
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
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

fn prepare(arguments: &[String]) -> Result<(String, Value), MachineError> {
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
    let result =
        validate_root(&input.root, &input.arguments).and_then(|_| prepare(&input.arguments));
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
