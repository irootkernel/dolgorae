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
                return render_failure(
                    cli.human,
                    cli.command.machine_name(),
                    MachineError::invalid_argument("argv", reason),
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
            command: RuntimeCommand::Capabilities
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
                    render_json(&SuccessEnvelope::new(command_name, data));
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
                    render_json(&SuccessEnvelope::new(command_name, data));
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
        return match dolgorae::review::execute_cli(&args.args) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed review result")
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
        return match dolgorae::specialist_policy::execute(operation, arguments) {
            Ok(data) => {
                if cli.human {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&data).expect("typed policy result")
                    );
                } else {
                    render_json(&SuccessEnvelope::new(command_name, data));
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
        return match dolgorae::review_target::execute(operation, arguments) {
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
            command: RuntimeCommand::Capabilities,
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
            if cli.human {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&data).expect("typed semantic result")
                );
            } else {
                render_json(&SuccessEnvelope::new(command_name, data));
            }
            ExitCode::SUCCESS
        }
        Err(error) => render_failure(cli.human, command_name, error),
    }
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
        RunCommand::Wait(leaf) => (SemanticRunVerb::Wait, leaf.args.as_slice()),
        RunCommand::Events(leaf) => (SemanticRunVerb::Events, leaf.args.as_slice()),
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
    let status = error.exit_status();
    if human {
        eprintln!("{}: {}", error.code, error.message);
    } else {
        let mut envelope = FailureEnvelope::new(command, error);
        if command.starts_with("runtime.orphan.") {
            envelope.schema_version = 3;
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
