#![forbid(unsafe_code)]

use clap::{CommandFactory, Parser};
use dolgorae::cli::{
    Cli, Command, ControllerCommand, ControllerCredentialCommand, EngagementCommand,
    OperatorCommand, OperatorCredentialCommand, ProfileCommand, ProfileDiagnosticsCommand,
    ProfileMembershipCommand, ProfileServerCommand, ProfileStateCommand, ReviewTargetCommand,
    RunCommand, RunControllerCommand, RuntimeCommand, RuntimeOrphanCommand, SpecialistCommand,
    SpecialistPolicyCommand, WorkspaceCommand, WorkspaceWriterCommand, option_path,
};
use dolgorae::machine::{FailureEnvelope, MachineError, SuccessEnvelope};
use dolgorae::semantic::{
    CoreSemanticService, RunVerb as SemanticRunVerb, SemanticCommand, SemanticResult,
    SemanticService, WorkspaceWriterVerb,
};
use dolgorae::workspace::WorkspaceMode;
use serde::Serialize;
use serde_json::json;
use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.len() == 4
        && args[1] == dolgorae::profile::PROFILE_LOG_DRAINER_COMMAND
        && args[2] == "--root"
    {
        return match dolgorae::profile::run_log_drainer(std::path::Path::new(&args[3])) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => ExitCode::from(error.exit_status()),
        };
    }
    let human = args.iter().any(|arg| arg == "--human");
    if is_help(&args) {
        return render_help(human);
    }
    if let Some(json_output) = version_output_mode(&args) {
        return render_version(json_output);
    }

    match Cli::try_parse_from(&args) {
        Ok(cli) => {
            if matches!(&cli.command, Command::Serve(_)) {
                return execute_serve(&cli.command);
            }
            if !matches!(&cli.command, Command::Profile { .. })
                && let Err(reason) = dolgorae::cli::validate_argument_contract(&cli.command)
            {
                return render_failure_version(
                    cli.human,
                    cli.command.machine_name(),
                    MachineError::invalid_argument("argv", reason),
                    uses_modern_contract(&cli.command),
                );
            }
            execute(cli)
        }
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            render_generated_help(human, error.to_string())
        }
        Err(error) => render_failure(
            human,
            "unknown",
            MachineError::invalid_argument("argv", error.to_string()),
        ),
    }
}

/// Serve owns a readiness channel, including pre-runtime argument and home
/// admission failures. It never enters the finite semantic-command renderer.
fn execute_serve(command: &Command) -> ExitCode {
    use std::os::fd::IntoRawFd as _;
    let Command::Serve(arguments) = command else {
        unreachable!("serve dispatcher receives only the serve command")
    };
    let ready_fd = match parse_ready_fd(&arguments.args) {
        Ok(fd) => fd,
        Err(error) => return render_ready_failure(None, error),
    };
    // Own the inherited descriptor before any home inspection can open files,
    // so an invalid descriptor number cannot accidentally name a new file.
    let ready_file = match ready_fd {
        Some(fd) => match dolgorae::darwin::DarwinSystem.take_ready_file(fd) {
            Ok(file) => Some(file),
            Err(_) => {
                return render_ready_failure(
                    None,
                    MachineError::invalid_argument(
                        "--ready-fd",
                        "readiness descriptor is unavailable",
                    ),
                );
            }
        },
        None => None,
    };
    let prepared = (|| {
        dolgorae::cli::validate_argument_contract(command)
            .map_err(|reason| MachineError::invalid_argument("argv", reason))?;
        let socket = option_path(&arguments.args, "--socket")
            .map_err(|reason| MachineError::invalid_argument("--socket", reason))?
            .ok_or_else(|| MachineError::invalid_argument("--socket", "socket path is required"))?;
        let home = dolgorae::paths::DolgoraeHome::system().map_err(MachineError::from)?;
        dolgorae::global_profile::require_generation(&home)?;
        Ok::<_, MachineError>((home, socket))
    })();
    let (home, socket) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return render_ready_failure(ready_file, error),
    };
    let backend_home = home.clone();
    let result = dolgorae::gateway::serve(
        &home,
        &socket,
        ready_file.map(|file| file.into_raw_fd()),
        move |instance_id| {
            std::sync::Arc::new(dolgorae::gateway_service::CoreGatewayBackend::new(
                backend_home,
                instance_id,
            ))
        },
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => ExitCode::from(error.exit_status()),
    }
}

fn parse_ready_fd(arguments: &[OsString]) -> Result<Option<i32>, MachineError> {
    let value = option_path(arguments, "--ready-fd")
        .map_err(|reason| MachineError::invalid_argument("--ready-fd", reason))?;
    value
        .map(|value| {
            value
                .to_str()
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|fd| *fd >= 0)
                .ok_or_else(|| {
                    MachineError::invalid_argument(
                        "--ready-fd",
                        "a nonnegative file descriptor is required",
                    )
                })
        })
        .transpose()
}

fn render_ready_failure(file: Option<std::fs::File>, error: MachineError) -> ExitCode {
    let status = error.exit_status();
    let mut output: Box<dyn std::io::Write> = match file {
        Some(file) => Box::new(file),
        None => Box::new(std::io::stdout()),
    };
    let envelope = FailureEnvelope::new("serve", error);
    if serde_json::to_writer(&mut output, &envelope).is_err()
        || output
            .write_all(b"\n")
            .and_then(|()| output.flush())
            .is_err()
    {
        return ExitCode::from(6);
    }
    ExitCode::from(status)
}

fn is_help(args: &[OsString]) -> bool {
    args.len() == 2 && (args[1] == "--help" || args[1] == "-h")
        || args.len() == 3 && args[1] == "--human" && (args[2] == "--help" || args[2] == "-h")
}

fn version_output_mode(args: &[OsString]) -> Option<bool> {
    match args.get(1..)? {
        [argument] if argument == "version" || argument == "--version" || argument == "-V" => {
            Some(false)
        }
        [command, argument] if command == "version" && argument == "--json" => Some(true),
        [human, argument]
            if human == "--human"
                && (argument == "version" || argument == "--version" || argument == "-V") =>
        {
            Some(false)
        }
        _ => None,
    }
}

fn render_help(human: bool) -> ExitCode {
    if human {
        Cli::command().print_long_help().expect("stdout");
        println!();
        ExitCode::SUCCESS
    } else {
        let mut bytes = Vec::new();
        Cli::command()
            .write_long_help(&mut bytes)
            .expect("memory write");
        render_json(&SuccessEnvelope::new(
            "help",
            json!({"text": String::from_utf8(bytes).expect("clap emits UTF-8")}),
        ));
        ExitCode::SUCCESS
    }
}

fn render_generated_help(human: bool, text: String) -> ExitCode {
    if human {
        print!("{text}");
    } else {
        render_json(&SuccessEnvelope::new("help", json!({"text": text})));
    }
    ExitCode::SUCCESS
}

#[derive(Serialize)]
struct VersionOutput<'a> {
    name: &'static str,
    version: &'a str,
}

fn render_version(json_output: bool) -> ExitCode {
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    if json_output {
        render_json(&VersionOutput {
            name: "dolgorae",
            version: &version,
        });
    } else {
        println!("dolgorae {version}");
    }
    ExitCode::SUCCESS
}

fn execute(cli: Cli) -> ExitCode {
    let command_name = cli.command.machine_name();
    if let Command::Maintenance { command } = &cli.command {
        return match dolgorae::maintenance::execute_native(command_name, &command.leaf_args().args)
        {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("maintenance response")
                    );
                } else {
                    render_json(
                        &json!({"schema":"dolgorae-machine/v5","ok":true,"command":command_name,"data":data}),
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Version(args) = &cli.command {
        if args.json && cli.human {
            return render_failure(
                cli.human,
                command_name,
                MachineError::invalid_argument("--json", "--json conflicts with --human"),
            );
        }
        return render_version(args.json);
    }
    if let Command::Profile { command } = &cli.command {
        let (operation, arguments) = profile_operation(command);
        if let Err(error) =
            dolgorae::global_profile::validate_post_cut_arguments(operation, arguments)
        {
            return render_failure(cli.human, command_name, error);
        }
    }
    if let Command::Runtime {
        command: RuntimeCommand::Capabilities(args),
    } = &cli.command
    {
        let result = option_path(&args.args, "--profile")
            .map_err(|reason| MachineError::invalid_argument("--profile", reason))
            .and_then(|profile| {
                let profile = profile
                    .map(|profile| {
                        profile.to_str().map(str::to_owned).ok_or_else(|| {
                            MachineError::invalid_argument(
                                "--profile",
                                "Profile name must be UTF-8",
                            )
                        })
                    })
                    .transpose()?;
                dolgorae::runtime::caller_capabilities(profile.as_deref())
            });
        return match result {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed capabilities")
                    );
                } else {
                    let mut envelope = SuccessEnvelope::new("capabilities", data);
                    envelope.schema_version = 4;
                    render_json(&envelope);
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, "capabilities", error),
        };
    }
    if let Command::Runtime {
        command: RuntimeCommand::Orphan { command },
    } = &cli.command
    {
        let (cleanup, arguments) = match command {
            RuntimeOrphanCommand::Inspect(args) => (false, &args.args),
            RuntimeOrphanCommand::Cleanup(args) => (true, &args.args),
        };
        return match dolgorae::process_inventory::execute(cleanup, arguments) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed orphan result")
                    );
                } else {
                    let mut envelope = SuccessEnvelope::new(command_name, data);
                    envelope.schema_version = 3;
                    render_json(&envelope);
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if !matches!(
        &cli.command,
        Command::Runtime {
            command: RuntimeCommand::Capabilities(_)
        } | Command::LiveTransportMcp(_)
    ) {
        let home = match dolgorae::paths::DolgoraeHome::system() {
            Ok(home) => home,
            Err(error) => return render_failure(cli.human, command_name, error.into()),
        };
        let generation = if matches!(&cli.command, Command::Init(_)) {
            dolgorae::global_profile::inspect_generation(&home).map(|_| ())
        } else {
            dolgorae::global_profile::require_generation(&home)
        };
        if let Err(error) = generation {
            return render_failure(cli.human, command_name, error);
        }
    }
    if let Command::LiveTransportMcp(args) = &cli.command {
        return match dolgorae::live_transport_mcp::ProbeProcessBinding::parse(
            &args.session_id,
            &args.run_id,
            args.generation,
            args.dedicated_lane,
        )
        .and_then(dolgorae::live_transport_mcp::serve_stdio)
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("dolgorae MCP startup failed: {}", error.code);
                ExitCode::from(error.exit_status())
            }
        };
    }
    if let Command::SpecialistReviewMcp(args) = &cli.command {
        return match dolgorae::mcp_review_server::serve_stdio(&args.workspace, &args.profile) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("dolgorae MCP startup failed: {}", error.code);
                ExitCode::from(error.exit_status())
            }
        };
    }
    if let Command::Instructions {
        command: dolgorae::cli::InstructionsCommand::Migrate(args),
    } = &cli.command
    {
        return match dolgorae::migration::execute_cli(&args.args) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed migration result")
                    );
                } else {
                    let mut envelope = SuccessEnvelope::new(command_name, data);
                    envelope.schema_version = 4;
                    render_json(&envelope);
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Engagement {
        command: EngagementCommand::Call(args),
    } = &cli.command
    {
        return match dolgorae::external_engagement::execute_cli(&args.args) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed engagement result")
                    );
                } else {
                    let modern = data["schema"] == "dolgorae-external-specialist-facade/v4";
                    let mut envelope = SuccessEnvelope::new(command_name, data);
                    if modern {
                        envelope.schema_version = 4;
                    }
                    render_json(&envelope);
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Worker(args) = &cli.command {
        #[cfg(target_os = "macos")]
        {
            return match dolgorae::worker::run_hidden_worker(&args.bootstrap) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    // The hidden worker answers on fd 3 with a code, never a
                    // machine envelope, so it reports the code alone rather
                    // than fabricating a Run identity to attach details to.
                    let code = error.code();
                    let _ = dolgorae::worker::write_startup_handoff(
                        &dolgorae::worker::StartupHandoff::Failed {
                            code: code.to_owned(),
                        },
                    );
                    ExitCode::from(dolgorae::machine::exit_status_for(code))
                }
            };
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = args;
            return ExitCode::from(6);
        }
    }
    if let Command::Run(run) = &cli.command
        && let RunCommand::List(args) = &run.command
    {
        return match dolgorae::semantic::run_list(&args.args) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed Run list")
                    );
                } else {
                    let modern = data
                        .get("items")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|items| {
                            items
                                .iter()
                                .any(|item| item.get("requested_access").is_some())
                        });
                    let mut envelope = SuccessEnvelope::new(command_name, data);
                    if modern {
                        envelope.schema_version = 4;
                    }
                    render_json(&envelope);
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Run(run) = &cli.command
        && let RunCommand::Artifact { command } = &run.command
        && let Some((args, read)) = match command {
            dolgorae::cli::RunArtifactCommand::Show(args) => Some((args, false)),
            dolgorae::cli::RunArtifactCommand::Read(args) => Some((args, true)),
            dolgorae::cli::RunArtifactCommand::Export(_) => None,
        }
    {
        return match dolgorae::semantic::run_artifact(&run_arguments(run, &args.args), read) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed artifact observation")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Run(run) = &cli.command
        && let RunCommand::Interaction {
            command: dolgorae::cli::RunInteractionCommand::Get(args),
        } = &run.command
    {
        let arguments = run_arguments(run, &args.args);
        return match dolgorae::semantic::interaction_get(&arguments) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed interaction")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Run(run) = &cli.command
        && let RunCommand::Controller {
            command: RunControllerCommand::Verify(args),
        } = &run.command
    {
        let mut arguments = args.args.clone();
        if let Some(path) = &run.controller_file {
            arguments.push(OsString::from("--controller-file"));
            arguments.push(path.as_os_str().to_owned());
        }
        if let Some(fd) = run.controller_fd {
            arguments.push(OsString::from("--controller-fd"));
            arguments.push(OsString::from(fd.to_string()));
        }
        return match dolgorae::controller::execute(
            dolgorae::controller::CredentialOperation::RunVerify,
            &arguments,
        ) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed controller verification")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Run(run) = &cli.command
        && let RunCommand::Controller {
            command: RunControllerCommand::Reset(args),
        } = &run.command
    {
        let mut arguments = args.args.clone();
        if let Some(path) = &run.operator_file {
            arguments.push(OsString::from("--operator-file"));
            arguments.push(path.as_os_str().to_owned());
        }
        if let Some(fd) = run.operator_fd {
            arguments.push(OsString::from("--operator-fd"));
            arguments.push(OsString::from(fd.to_string()));
        }
        return match dolgorae::controller::reset_run(&arguments, &dolgorae::semantic::LiveRunReset)
        {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed controller reset")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    let credential = match &cli.command {
        Command::Controller {
            command:
                ControllerCommand::Credential {
                    command: ControllerCredentialCommand::Create(args),
                },
        } => Some((
            dolgorae::controller::CredentialOperation::ControllerCreate,
            &args.args,
        )),
        Command::Operator {
            command: OperatorCommand::Credential { command },
        } => Some(match command {
            OperatorCredentialCommand::Initialize(args) => (
                dolgorae::controller::CredentialOperation::OperatorInitialize,
                &args.args,
            ),
            OperatorCredentialCommand::Rotate(args) => (
                dolgorae::controller::CredentialOperation::OperatorRotate,
                &args.args,
            ),
        }),
        _ => None,
    };
    if let Some((operation, arguments)) = credential {
        return match dolgorae::controller::execute(operation, arguments) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed credential result")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Profile { command } = &cli.command {
        let (operation, arguments) = profile_operation(command);
        return match dolgorae::profile::execute_global_with_member_quiescer(
            operation,
            arguments,
            dolgorae::semantic::quiesce_profile_member,
        ) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed profile result")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Specialist {
        command: SpecialistCommand::Review(args),
    } = &cli.command
    {
        return match dolgorae::review::execute_direct_cli(&args.args) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed review result")
                    );
                } else {
                    let mut envelope = SuccessEnvelope::new(command_name, data);
                    envelope.schema_version = 4;
                    render_json(&envelope);
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Specialist {
        command: SpecialistCommand::ReviewInspect(args) | SpecialistCommand::ReviewRecover(args),
    } = &cli.command
    {
        return match dolgorae::review_recovery::execute_cli(command_name, &args.args) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed review observation")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Specialist {
        command: SpecialistCommand::Policy { command },
    } = &cli.command
    {
        let (operation, arguments) = match command {
            SpecialistPolicyCommand::Add(args) => (
                dolgorae::specialist_policy::PolicyOperation::Add,
                &args.args,
            ),
            SpecialistPolicyCommand::List(args) => (
                dolgorae::specialist_policy::PolicyOperation::List,
                &args.args,
            ),
            SpecialistPolicyCommand::Show(args) => (
                dolgorae::specialist_policy::PolicyOperation::Show,
                &args.args,
            ),
            SpecialistPolicyCommand::Validate(args) => (
                dolgorae::specialist_policy::PolicyOperation::Validate,
                &args.args,
            ),
            SpecialistPolicyCommand::Remove(args) => (
                dolgorae::specialist_policy::PolicyOperation::Remove,
                &args.args,
            ),
        };
        return match dolgorae::markdown_policy::execute(operation, arguments) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed policy result")
                    );
                } else {
                    let modern = data.get("policy").is_some()
                        || data
                            .get("items")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|items| {
                                items.iter().any(|item| item.get("policy").is_some())
                            });
                    let mut envelope = SuccessEnvelope::new(command_name, data);
                    if modern {
                        envelope.schema_version = 4;
                    }
                    render_json(&envelope);
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::ReviewTarget { command } = &cli.command {
        let (operation, arguments) = match command {
            ReviewTargetCommand::Capture(args) => {
                (dolgorae::review_target::Operation::Capture, &args.args)
            }
            ReviewTargetCommand::Settle(args) => {
                (dolgorae::review_target::Operation::Settle, &args.args)
            }
        };
        return match dolgorae::review_target::execute_current(operation, arguments) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed review target result")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    if let Command::Workspace {
        command: WorkspaceCommand::Writer { command },
    } = &cli.command
    {
        let (operation, arguments) = match command {
            WorkspaceWriterCommand::Status(args) => (WorkspaceWriterVerb::Status, &args.args),
            WorkspaceWriterCommand::Reset(args) => (WorkspaceWriterVerb::Reset, &args.args),
            WorkspaceWriterCommand::HandoffPrepare(args) => {
                (WorkspaceWriterVerb::HandoffPrepare, &args.args)
            }
            WorkspaceWriterCommand::HandoffCommit(args) => {
                (WorkspaceWriterVerb::HandoffCommit, &args.args)
            }
            WorkspaceWriterCommand::HandoffCancel(args) => {
                (WorkspaceWriterVerb::HandoffCancel, &args.args)
            }
        };
        return match dolgorae::semantic::workspace_writer(operation, arguments) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed writer result")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
                ExitCode::SUCCESS
            }
            Err(error) => render_failure(cli.human, command_name, error),
        };
    }
    let semantic_command = match &cli.command {
        Command::Worker(_) => unreachable!("hidden worker handled before semantic dispatch"),
        Command::SpecialistReviewMcp(_) => {
            unreachable!("hidden MCP server handled before semantic dispatch")
        }
        Command::LiveTransportMcp(_) => {
            unreachable!("hidden live-transport MCP probe handled before semantic dispatch")
        }
        Command::Version(_) => unreachable!("version handled before semantic dispatch"),
        Command::ReviewTarget { .. } => {
            unreachable!("review target handled before semantic dispatch")
        }
        Command::Runtime {
            command: RuntimeCommand::Capabilities(_),
        } => SemanticCommand::RuntimeCapabilities,
        Command::Runtime {
            command: RuntimeCommand::Orphan { .. },
        } => unreachable!("orphan handled before semantic dispatch"),
        Command::Init(args) => SemanticCommand::Initialize {
            path: args.path.clone(),
            mode: if args.non_git {
                WorkspaceMode::NonGit
            } else {
                WorkspaceMode::Git
            },
        },
        Command::Workspace {
            command: WorkspaceCommand::Inspect(args),
        } => match option_path(&args.args, "--workspace") {
            Ok(workspace) => SemanticCommand::WorkspaceInspect { workspace },
            Err(reason) => {
                return render_failure(
                    cli.human,
                    command_name,
                    MachineError::invalid_argument("--workspace", reason),
                );
            }
        },
        Command::Workspace {
            command: WorkspaceCommand::Writer { .. },
        } => unreachable!("workspace writer handled before semantic dispatch"),
        Command::Run(run) if run_verb(&run.command).is_some() => {
            let (verb, leaf) = run_verb(&run.command).expect("guard proves a supported run verb");
            SemanticCommand::Run {
                verb,
                args: run_arguments(run, leaf),
            }
        }
        Command::Profile { .. } => unreachable!("profile handled before semantic dispatch"),
        _ => SemanticCommand::Future {
            dotted_name: command_name.to_owned(),
        },
    };
    let service = CoreSemanticService;
    match service.execute(&semantic_command) {
        // `run events` is the one command family that emits more than one
        // machine object: SPEC-006 has it emit one envelope per durable record
        // through the head captured at command start, then the `end` frame.
        Ok(SemanticResult::RunStream(objects)) => {
            for data in objects {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed semantic result")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
                }
            }
            ExitCode::SUCCESS
        }
        Ok(result) => {
            let data = serde_json::to_value(result).expect("typed semantic result");
            if command_name == "run.lookup_operation" && !cli.human {
                render_json(
                    &json!({"schema":"dolgorae-machine/v5","ok":true,"command":"lookup_operation",
                    "data":{"schema":"dolgorae.external-specialist-response/v5","operation":"lookup_operation","lookup":data}}),
                );
                return ExitCode::SUCCESS;
            }
            if cli.human {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&data).expect("typed semantic result")
                );
            } else {
                let modern = data.get("requested_access").is_some()
                    || data
                        .get("items")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|items| {
                            items
                                .iter()
                                .any(|item| item.get("requested_access").is_some())
                        });
                let name = match (modern, command_name) {
                    (true, "run.acquire_write") => "run.acquire-write",
                    (true, "run.release_write") => "run.release-write",
                    _ => command_name,
                };
                let mut envelope = SuccessEnvelope::new(name, data);
                if modern {
                    envelope.schema_version = 4;
                }
                render_json(&envelope);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            let modern = uses_modern_contract(&cli.command);
            render_failure_version(cli.human, command_name, error, modern)
        }
    }
}

fn uses_modern_contract(command: &Command) -> bool {
    matches!(command, Command::Run(run)
        if matches!(&run.command, RunCommand::Start(args)
            if !args.args.iter().any(|arg| arg == "--execution-lane")))
        || matches!(
            command,
            Command::Instructions { .. }
                | Command::Runtime {
                    command: RuntimeCommand::Capabilities(_)
                }
                | Command::Specialist {
                    command: SpecialistCommand::Review(_)
                }
                | Command::Specialist {
                    command: SpecialistCommand::Policy {
                        command: SpecialistPolicyCommand::Add(_)
                            | SpecialistPolicyCommand::Validate(_)
                    }
                }
        )
}

fn profile_operation(
    command: &ProfileCommand,
) -> (dolgorae::profile::ProfileOperation, &[OsString]) {
    match command {
        ProfileCommand::Add(args) => (dolgorae::profile::ProfileOperation::Add, &args.args),
        ProfileCommand::List(args) => (dolgorae::profile::ProfileOperation::List, &args.args),
        ProfileCommand::Show(args) => (dolgorae::profile::ProfileOperation::Show, &args.args),
        ProfileCommand::Remove(args) => (dolgorae::profile::ProfileOperation::Remove, &args.args),
        ProfileCommand::Doctor(args) => (dolgorae::profile::ProfileOperation::Doctor, &args.args),
        ProfileCommand::Server { command } => match command {
            ProfileServerCommand::Status(args) => (
                dolgorae::profile::ProfileOperation::ServerStatus,
                &args.args,
            ),
            ProfileServerCommand::Start(args) => {
                (dolgorae::profile::ProfileOperation::ServerStart, &args.args)
            }
            ProfileServerCommand::Stop(args) => {
                (dolgorae::profile::ProfileOperation::ServerStop, &args.args)
            }
            ProfileServerCommand::Restart(args) => (
                dolgorae::profile::ProfileOperation::ServerRestart,
                &args.args,
            ),
            ProfileServerCommand::Migrate(args) => (
                dolgorae::profile::ProfileOperation::ServerMigrate,
                &args.args,
            ),
        },
        ProfileCommand::Membership { command } => match command {
            ProfileMembershipCommand::Verify(args) => (
                dolgorae::profile::ProfileOperation::MembershipVerify,
                &args.args,
            ),
            ProfileMembershipCommand::TombstoneOrphan(args) => (
                dolgorae::profile::ProfileOperation::MembershipTombstoneOrphan,
                &args.args,
            ),
        },
        ProfileCommand::State {
            command: ProfileStateCommand::Reset(args),
        } => (dolgorae::profile::ProfileOperation::StateReset, &args.args),
        ProfileCommand::Diagnostics {
            command: ProfileDiagnosticsCommand::List(args),
        } => (
            dolgorae::profile::ProfileOperation::DiagnosticsList,
            &args.args,
        ),
        ProfileCommand::Events(args) => (dolgorae::profile::ProfileOperation::Events, &args.args),
    }
}

/// The `run` verbs this slice executes, paired with their leaf arguments; every
/// other verb still resolves to the roadmap-owned future command.
fn run_verb(command: &RunCommand) -> Option<(SemanticRunVerb, &[OsString])> {
    Some(match command {
        RunCommand::Start(leaf) => (SemanticRunVerb::Start, leaf.args.as_slice()),
        RunCommand::Status(leaf) => (SemanticRunVerb::Status, leaf.args.as_slice()),
        RunCommand::Send(leaf) => (SemanticRunVerb::Send, leaf.args.as_slice()),
        RunCommand::Submit(leaf) => (SemanticRunVerb::Submit, leaf.args.as_slice()),
        RunCommand::LookupOperation(leaf) => {
            (SemanticRunVerb::LookupOperation, leaf.args.as_slice())
        }
        RunCommand::Wait(leaf) => (SemanticRunVerb::Wait, leaf.args.as_slice()),
        RunCommand::Events(leaf) => (SemanticRunVerb::Events, leaf.args.as_slice()),
        RunCommand::Timeline(leaf) => (SemanticRunVerb::Timeline, leaf.args.as_slice()),
        RunCommand::Pending(leaf) => (SemanticRunVerb::Pending, leaf.args.as_slice()),
        RunCommand::Respond(leaf) => (SemanticRunVerb::Respond, leaf.args.as_slice()),
        RunCommand::Interrupt(leaf) => (SemanticRunVerb::Interrupt, leaf.args.as_slice()),
        RunCommand::Pause(leaf) => (SemanticRunVerb::Pause, leaf.args.as_slice()),
        RunCommand::Resume(leaf) => (SemanticRunVerb::Resume, leaf.args.as_slice()),
        RunCommand::Recover(leaf) => (SemanticRunVerb::Recover, leaf.args.as_slice()),
        RunCommand::Reconcile(leaf) => (SemanticRunVerb::Reconcile, leaf.args.as_slice()),
        RunCommand::Fork(leaf) => (SemanticRunVerb::Fork, leaf.args.as_slice()),
        RunCommand::AcquireWrite(leaf) => (SemanticRunVerb::AcquireWrite, leaf.args.as_slice()),
        RunCommand::ReleaseWrite(leaf) => (SemanticRunVerb::ReleaseWrite, leaf.args.as_slice()),
        RunCommand::CreateWriteContinuation(leaf) => (
            SemanticRunVerb::CreateWriteContinuation,
            leaf.args.as_slice(),
        ),
        RunCommand::Close(leaf) => (SemanticRunVerb::Close, leaf.args.as_slice()),
        _ => return None,
    })
}

/// Merge the credential carrier options that sit on the `run` group into the
/// leaf arguments, so one parser sees the whole request.
fn run_arguments(run: &dolgorae::cli::RunArgs, leaf: &[OsString]) -> Vec<OsString> {
    let mut arguments = leaf.to_vec();
    if let Some(path) = &run.controller_file {
        arguments.push(OsString::from("--controller-file"));
        arguments.push(path.as_os_str().to_owned());
    }
    if let Some(fd) = run.controller_fd {
        arguments.push(OsString::from("--controller-fd"));
        arguments.push(OsString::from(fd.to_string()));
    }
    arguments
}

fn render_failure(human: bool, command: &str, error: MachineError) -> ExitCode {
    let modern = matches!(
        command,
        "instructions.migrate"
            | "capabilities"
            | "specialist.review"
            | "specialist.policy.add"
            | "specialist.policy.validate"
    );
    render_failure_version(human, command, error, modern)
}

fn render_failure_version(
    human: bool,
    command: &str,
    error: MachineError,
    modern: bool,
) -> ExitCode {
    let status = error.exit_status();
    if matches!(
        command,
        "plan_maintenance" | "apply_maintenance" | "get_maintenance"
    ) && !human
    {
        let operation_ref = error
            .details
            .get("operation_ref")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let unresolved = error.details["effect"] == "unresolved";
        let recovery = matches!(
            error.code.as_str(),
            "RECOVERY_REQUIRED"
                | "ARTIFACT_INTEGRITY_FAILURE"
                | "LOOKUP_NAMESPACE_UNAVAILABLE"
                | "LOOKUP_NAMESPACE_CORRUPT"
        );
        render_json(
            &json!({"schema":"dolgorae-machine/v5","ok":false,"command":command,
            "error":{"schema":"dolgorae.successor-error/v1","code":error.code,"operation_kind":command,"operation_ref":operation_ref,
                "effect":if unresolved || recovery {"unresolved"} else {"none"},"classification":if unresolved || recovery {"RecoveryNeeded"} else {"Rejected"},
                "required_action":if unresolved && error.details["required_action"] != "restore_state" {"resume_maintenance"} else if unresolved || recovery {"restore_state"} else {"fix_request"},"retry_new_key":false,"subjects":null}}),
        );
        return ExitCode::from(status);
    }
    if command == "run.lookup_operation" && !human {
        let (effect, classification, action) = match error.code.as_str() {
            "AUDIT_INTEGRITY_FAILURE" | "RECOVERY_REQUIRED" => {
                ("unresolved", "RecoveryNeeded", "restore_state")
            }
            "INTERNAL_ERROR" => ("unresolved", "Unknown", "lookup_original_key"),
            _ => ("none", "Rejected", "fix_request"),
        };
        let subjects = matches!(
            error.code.as_str(),
            "AUDIT_INTEGRITY_FAILURE" | "RUN_STATE_CONFLICT"
        )
        .then(|| {
            error
                .details
                .get("run_id")
                .and_then(serde_json::Value::as_str)
        })
        .flatten()
        .map(|run| dolgorae::operation::Subjects {
            run_ref: Some(run.to_owned()),
            ..Default::default()
        });
        render_json(
            &json!({"schema":"dolgorae-machine/v5","ok":false,"command":"lookup_operation",
            "error":{"schema":"dolgorae.successor-error/v1","code":error.code,
                "operation_kind":"lookup_operation","operation_ref":null,"effect":effect,
                "classification":classification,"required_action":action,"retry_new_key":false,"subjects":subjects}}),
        );
        return ExitCode::from(status);
    }
    if human {
        eprintln!("{}: {}", error.code, error.message);
    } else {
        let mut envelope = FailureEnvelope::new(command, error);
        if command.starts_with("runtime.orphan.") {
            envelope.schema_version = 3;
        }
        if modern
            || envelope.error.details["schema"] == "dolgorae-session-close-error-details/v1"
            || matches!(
                envelope.error.code.as_str(),
                "SOURCE_ISOLATION_RETIRED"
                    | "ROLE_MIGRATION_REQUIRED"
                    | "INSTRUCTION_SOURCE_INVALID"
                    | "INSTRUCTION_DISCOVERY_UNSUPPORTED"
                    | "WORKSPACE_STARTUP_UNSUPPORTED"
                    | "REVIEW_READING_BOUNDARY_UNSUPPORTED"
                    | "REVIEW_CANDIDATE_PROVENANCE_INVALID"
                    | "REVIEW_TARGET_DRIFTED"
                    | "REVIEW_TARGET_UNAVAILABLE"
            )
        {
            envelope.schema_version = 4;
        }
        if envelope.schema_version == 4 {
            envelope.command = match envelope.command.as_str() {
                "run.acquire_write" => "run.acquire-write".to_owned(),
                "run.release_write" => "run.release-write".to_owned(),
                "runtime.capabilities" => "capabilities".to_owned(),
                _ => envelope.command,
            };
        }
        render_json(&envelope);
    }
    ExitCode::from(status)
}

fn render_json(value: &impl Serialize) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, value).expect("machine envelope serialization");
    lock.write_all(b"\n").expect("machine newline");
}

#[cfg(test)]
mod gateway_cli_tests {
    use super::*;
    use std::io::Read as _;
    use std::os::fd::IntoRawFd as _;
    use std::time::Duration;

    #[test]
    fn capacity_failure_renderer_child() {
        if let Ok(command) = std::env::var("DOLGORAE_TEST_CAPACITY_COMMAND") {
            println!();
            let status = render_failure_version(
                false,
                &command,
                MachineError::new(
                    "STORAGE_CAPACITY_EXCEEDED",
                    "selected admission full",
                    false,
                    json!({"required_action":"operator_checkpoint"}),
                ),
                false,
            );
            assert_eq!(status, ExitCode::from(4));
            std::process::exit(4);
        }
    }
    #[test]
    fn run_cli_capacity_failure_matches_the_frozen_machine_contract() {
        for command in ["run.send", "run.submit"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gateway_cli_tests::capacity_failure_renderer_child",
                    "--nocapture",
                ])
                .env("DOLGORAE_TEST_CAPACITY_COMMAND", command)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(4));
            let stdout = String::from_utf8(output.stdout).unwrap();
            let envelope: serde_json::Value =
                serde_json::from_str(stdout.lines().find(|l| l.starts_with('{')).unwrap()).unwrap();
            assert_eq!(envelope["command"], command);
            assert_eq!(envelope["error"]["code"], "CAPABILITY_UNSUPPORTED");
            assert_eq!(
                envelope["error"]["details"]["capability_name"],
                "dolgorae.storage-capacity/v014"
            );
            let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let mut checker = std::process::Command::new(repository.join(".venv/bin/python"))
                .args(["-B", "-c", "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path(sys.argv[1])/'tests/e2e')); from schema_support import validator,assert_valid; assert_valid(json.load(sys.stdin),validator(pathlib.Path(sys.argv[1])/'docs/protocol','dolgorae-machine-v2.schema.json'),'Run capacity failure')"])
                .arg(repository).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
            checker
                .stdin
                .take()
                .unwrap()
                .write_all(&serde_json::to_vec(&envelope).unwrap())
                .unwrap();
            let checked = checker.wait_with_output().unwrap();
            assert!(
                checked.status.success(),
                "{}",
                String::from_utf8_lossy(&checked.stderr)
            );
        }
    }

    #[test]
    fn serve_keeps_the_exact_foreground_socket_syntax() {
        let cli = Cli::try_parse_from([
            "dolgorae",
            "serve",
            "--socket",
            "/private/g.sock",
            "--ready-fd=7",
        ])
        .unwrap();
        dolgorae::cli::validate_argument_contract(&cli.command).unwrap();
        let Command::Serve(arguments) = cli.command else {
            panic!("expected serve");
        };
        assert_eq!(parse_ready_fd(&arguments.args).unwrap(), Some(7));
        for args in [
            vec!["dolgorae", "serve", "rpc", "--socket", "/private/g.sock"],
            vec![
                "dolgorae",
                "serve",
                "--socket",
                "/private/g.sock",
                "--tcp",
                "127.0.0.1:1",
            ],
            vec![
                "dolgorae",
                "serve",
                "--socket",
                "/private/g.sock",
                "--daemonize",
            ],
            vec!["dolgorae", "serve", "--ready-fd", "7"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(dolgorae::cli::validate_argument_contract(&cli.command).is_err());
        }
    }

    #[test]
    fn malformed_ready_descriptors_are_rejected_before_adoption() {
        for value in ["-1", "not-a-descriptor", "2147483648"] {
            assert!(parse_ready_fd(&["--ready-fd".into(), value.into()]).is_err());
        }
        assert!(parse_ready_fd(&["--ready-fd=7".into(), "--ready-fd=8".into()]).is_err());
    }

    #[test]
    fn serve_argument_failure_uses_the_inherited_readiness_channel_once() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let cli = Cli::try_parse_from(vec![
            "dolgorae".to_owned(),
            "serve".to_owned(),
            "--ready-fd".to_owned(),
            writer.into_raw_fd().to_string(),
        ])
        .unwrap();
        let _ = execute_serve(&cli.command);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
        let envelope: FailureEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(envelope.command, "serve");
        assert_eq!(envelope.error.code, "INVALID_ARGUMENT");
    }
}
