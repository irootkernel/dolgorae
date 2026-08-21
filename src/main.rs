#![forbid(unsafe_code)]

use clap::{CommandFactory, Parser};
use dolgorae::cli::{Cli, Command, RunCommand, RuntimeCommand, WorkspaceCommand, option_path};
use dolgorae::machine::{FailureEnvelope, MachineError, SuccessEnvelope};
use dolgorae::semantic::{CoreSemanticService, SemanticCommand, SemanticService};
use dolgorae::workspace::WorkspaceMode;
use serde::Serialize;
use serde_json::json;
use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args_os().collect::<Vec<_>>();
    let human = args.iter().any(|arg| arg == "--human");
    if is_help(&args) {
        return render_help(human);
    }
    if is_version(&args) {
        return render_version(human);
    }

    match Cli::try_parse_from(&args) {
        Ok(cli) => {
            if let Err(reason) = dolgorae::cli::validate_argument_contract(&cli.command) {
                return render_failure(
                    cli.human,
                    cli.command.machine_name(),
                    MachineError::invalid_argument("argv", reason),
                );
            }
            execute(cli)
        }
        Err(error) => render_failure(
            human,
            "unknown",
            MachineError::invalid_argument("argv", error.to_string()),
        ),
    }
}

fn is_help(args: &[OsString]) -> bool {
    args.len() == 2 && (args[1] == "--help" || args[1] == "-h")
        || args.len() == 3 && args[1] == "--human" && (args[2] == "--help" || args[2] == "-h")
}

fn is_version(args: &[OsString]) -> bool {
    args.len() == 2 && (args[1] == "--version" || args[1] == "-V")
        || args.len() == 3 && args[1] == "--human" && (args[2] == "--version" || args[2] == "-V")
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

fn render_version(human: bool) -> ExitCode {
    if human {
        println!("dolgorae {}", env!("CARGO_PKG_VERSION"));
    } else {
        render_json(&SuccessEnvelope::new(
            "version",
            json!({"text": format!("dolgorae {}", env!("CARGO_PKG_VERSION"))}),
        ));
    }
    ExitCode::SUCCESS
}

fn execute(cli: Cli) -> ExitCode {
    let command_name = cli.command.machine_name();
    let semantic_command = match &cli.command {
        Command::Runtime {
            command: RuntimeCommand::Capabilities,
        } => SemanticCommand::RuntimeCapabilities,
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
        Command::Run(run) if matches!(run.command, RunCommand::Start(_)) => {
            let RunCommand::Start(leaf) = &run.command else {
                unreachable!("guard proves run start")
            };
            match option_path(&leaf.args, "--workspace") {
                Ok(workspace) => SemanticCommand::RunStartPreflight { workspace },
                Err(reason) => {
                    return render_failure(
                        cli.human,
                        command_name,
                        MachineError::invalid_argument("--workspace", reason),
                    );
                }
            }
        }
        _ => SemanticCommand::Future {
            dotted_name: command_name.to_owned(),
        },
    };
    let service = CoreSemanticService;
    match service.execute(&semantic_command) {
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

fn render_failure(human: bool, command: &str, error: MachineError) -> ExitCode {
    let status = error.exit_status();
    if human {
        eprintln!("{}: {}", error.code, error.message);
    } else {
        render_json(&FailureEnvelope::new(command, error));
    }
    ExitCode::from(status)
}

fn render_json(value: &impl Serialize) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, value).expect("machine envelope serialization");
    lock.write_all(b"\n").expect("machine newline");
}
