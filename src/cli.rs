use clap::{Args, Parser, Subcommand};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "dolgorae",
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub human: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init(InitArgs),
    Serve(LeafArgs),
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    Engagement {
        #[command(subcommand)]
        command: EngagementCommand,
    },
    Controller {
        #[command(subcommand)]
        command: ControllerCommand,
    },
    Operator {
        #[command(subcommand)]
        command: OperatorCommand,
    },
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    Specialist {
        #[command(subcommand)]
        command: SpecialistCommand,
    },
    Run(RunArgs),
}

impl Command {
    #[must_use]
    pub const fn machine_name(&self) -> &'static str {
        match self {
            Self::Init(_) => "init",
            Self::Serve(_) => "serve",
            Self::Runtime { command } => command.machine_name(),
            Self::Engagement { command } => command.machine_name(),
            Self::Controller { command } => command.machine_name(),
            Self::Operator { command } => command.machine_name(),
            Self::Workspace { command } => command.machine_name(),
            Self::Profile { command } => command.machine_name(),
            Self::Specialist { command } => command.machine_name(),
            Self::Run(args) => args.command.machine_name(),
        }
    }

    #[must_use]
    pub fn leaf_args(&self) -> Option<&LeafArgs> {
        match self {
            Self::Init(_) | Self::Runtime { .. } => None,
            Self::Serve(args) => Some(args),
            Self::Engagement {
                command: EngagementCommand::Call(args),
            } => Some(args),
            Self::Controller {
                command:
                    ControllerCommand::Credential {
                        command: ControllerCredentialCommand::Create(args),
                    },
            } => Some(args),
            Self::Operator {
                command: OperatorCommand::Credential { command },
            } => match command {
                OperatorCredentialCommand::Initialize(args)
                | OperatorCredentialCommand::Rotate(args) => Some(args),
            },
            Self::Workspace { command } => command.leaf_args(),
            Self::Profile { command } => command.leaf_args(),
            Self::Specialist { command } => command.leaf_args(),
            Self::Run(args) => args.command.leaf_args(),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum RuntimeCommand {
    Capabilities,
}
impl RuntimeCommand {
    const fn machine_name(&self) -> &'static str {
        "runtime.capabilities"
    }
}

#[derive(Debug, Subcommand)]
pub enum EngagementCommand {
    Call(LeafArgs),
}
impl EngagementCommand {
    const fn machine_name(&self) -> &'static str {
        "engagement.call"
    }
}

#[derive(Debug, Subcommand)]
pub enum ControllerCommand {
    Credential {
        #[command(subcommand)]
        command: ControllerCredentialCommand,
    },
}
#[derive(Debug, Subcommand)]
pub enum ControllerCredentialCommand {
    Create(LeafArgs),
}
impl ControllerCommand {
    const fn machine_name(&self) -> &'static str {
        "controller.credential.create"
    }
}

#[derive(Debug, Subcommand)]
pub enum OperatorCommand {
    Credential {
        #[command(subcommand)]
        command: OperatorCredentialCommand,
    },
}
#[derive(Debug, Subcommand)]
pub enum OperatorCredentialCommand {
    Initialize(LeafArgs),
    Rotate(LeafArgs),
}
impl OperatorCommand {
    const fn machine_name(&self) -> &'static str {
        match self {
            Self::Credential {
                command: OperatorCredentialCommand::Initialize(_),
            } => "operator.credential.initialize",
            Self::Credential {
                command: OperatorCredentialCommand::Rotate(_),
            } => "operator.credential.rotate",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    Inspect(LeafArgs),
    Writer {
        #[command(subcommand)]
        command: WorkspaceWriterCommand,
    },
}
#[derive(Debug, Subcommand)]
pub enum WorkspaceWriterCommand {
    Status(LeafArgs),
    Reset(LeafArgs),
    HandoffPrepare(LeafArgs),
    HandoffCommit(LeafArgs),
    HandoffCancel(LeafArgs),
}
impl WorkspaceCommand {
    const fn machine_name(&self) -> &'static str {
        match self {
            Self::Inspect(_) => "workspace.inspect",
            Self::Writer { command } => match command {
                WorkspaceWriterCommand::Status(_) => "workspace.writer.status",
                WorkspaceWriterCommand::Reset(_) => "workspace.writer.reset",
                WorkspaceWriterCommand::HandoffPrepare(_) => "workspace.writer.handoff_prepare",
                WorkspaceWriterCommand::HandoffCommit(_) => "workspace.writer.handoff_commit",
                WorkspaceWriterCommand::HandoffCancel(_) => "workspace.writer.handoff_cancel",
            },
        }
    }
    fn leaf_args(&self) -> Option<&LeafArgs> {
        Some(match self {
            Self::Inspect(args) => args,
            Self::Writer { command } => match command {
                WorkspaceWriterCommand::Status(args)
                | WorkspaceWriterCommand::Reset(args)
                | WorkspaceWriterCommand::HandoffPrepare(args)
                | WorkspaceWriterCommand::HandoffCommit(args)
                | WorkspaceWriterCommand::HandoffCancel(args) => args,
            },
        })
    }
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    Add(LeafArgs),
    List(LeafArgs),
    Show(LeafArgs),
    Remove(LeafArgs),
    Doctor(LeafArgs),
    Server {
        #[command(subcommand)]
        command: ProfileServerCommand,
    },
    Membership {
        #[command(subcommand)]
        command: ProfileMembershipCommand,
    },
    State {
        #[command(subcommand)]
        command: ProfileStateCommand,
    },
    Diagnostics {
        #[command(subcommand)]
        command: ProfileDiagnosticsCommand,
    },
    Events(LeafArgs),
}
#[derive(Debug, Subcommand)]
pub enum ProfileServerCommand {
    Status(LeafArgs),
    Start(LeafArgs),
    Stop(LeafArgs),
    Restart(LeafArgs),
    Migrate(LeafArgs),
}
#[derive(Debug, Subcommand)]
pub enum ProfileMembershipCommand {
    Verify(LeafArgs),
    TombstoneOrphan(LeafArgs),
}
#[derive(Debug, Subcommand)]
pub enum ProfileStateCommand {
    Reset(LeafArgs),
}
#[derive(Debug, Subcommand)]
pub enum ProfileDiagnosticsCommand {
    List(LeafArgs),
}
impl ProfileCommand {
    const fn machine_name(&self) -> &'static str {
        match self {
            Self::Add(_) => "profile.add",
            Self::List(_) => "profile.list",
            Self::Show(_) => "profile.show",
            Self::Remove(_) => "profile.remove",
            Self::Doctor(_) => "profile.doctor",
            Self::Events(_) => "profile.events",
            Self::Server { command } => match command {
                ProfileServerCommand::Status(_) => "profile.server.status",
                ProfileServerCommand::Start(_) => "profile.server.start",
                ProfileServerCommand::Stop(_) => "profile.server.stop",
                ProfileServerCommand::Restart(_) => "profile.server.restart",
                ProfileServerCommand::Migrate(_) => "profile.server.migrate",
            },
            Self::Membership { command } => match command {
                ProfileMembershipCommand::Verify(_) => "profile.membership.verify",
                ProfileMembershipCommand::TombstoneOrphan(_) => {
                    "profile.membership.tombstone_orphan"
                }
            },
            Self::State { .. } => "profile.state.reset",
            Self::Diagnostics { .. } => "profile.diagnostics.list",
        }
    }
    fn leaf_args(&self) -> Option<&LeafArgs> {
        Some(match self {
            Self::Add(a)
            | Self::List(a)
            | Self::Show(a)
            | Self::Remove(a)
            | Self::Doctor(a)
            | Self::Events(a) => a,
            Self::Server { command } => match command {
                ProfileServerCommand::Status(a)
                | ProfileServerCommand::Start(a)
                | ProfileServerCommand::Stop(a)
                | ProfileServerCommand::Restart(a)
                | ProfileServerCommand::Migrate(a) => a,
            },
            Self::Membership { command } => match command {
                ProfileMembershipCommand::Verify(a)
                | ProfileMembershipCommand::TombstoneOrphan(a) => a,
            },
            Self::State {
                command: ProfileStateCommand::Reset(a),
            }
            | Self::Diagnostics {
                command: ProfileDiagnosticsCommand::List(a),
            } => a,
        })
    }
}

#[derive(Debug, Subcommand)]
pub enum SpecialistCommand {
    Review(LeafArgs),
    Policy {
        #[command(subcommand)]
        command: SpecialistPolicyCommand,
    },
}
#[derive(Debug, Subcommand)]
pub enum SpecialistPolicyCommand {
    Add(LeafArgs),
    List(LeafArgs),
    Show(LeafArgs),
    Validate(LeafArgs),
    Remove(LeafArgs),
}
impl SpecialistCommand {
    const fn machine_name(&self) -> &'static str {
        match self {
            Self::Review(_) => "specialist.review",
            Self::Policy { command } => match command {
                SpecialistPolicyCommand::Add(_) => "specialist.policy.add",
                SpecialistPolicyCommand::List(_) => "specialist.policy.list",
                SpecialistPolicyCommand::Show(_) => "specialist.policy.show",
                SpecialistPolicyCommand::Validate(_) => "specialist.policy.validate",
                SpecialistPolicyCommand::Remove(_) => "specialist.policy.remove",
            },
        }
    }
    fn leaf_args(&self) -> Option<&LeafArgs> {
        Some(match self {
            Self::Review(a) => a,
            Self::Policy { command } => match command {
                SpecialistPolicyCommand::Add(a)
                | SpecialistPolicyCommand::List(a)
                | SpecialistPolicyCommand::Show(a)
                | SpecialistPolicyCommand::Validate(a)
                | SpecialistPolicyCommand::Remove(a) => a,
            },
        })
    }
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long, conflicts_with = "controller_fd")]
    pub controller_file: Option<PathBuf>,
    #[arg(long, conflicts_with = "controller_file")]
    pub controller_fd: Option<i32>,
    #[arg(long, conflicts_with = "operator_fd")]
    pub operator_file: Option<PathBuf>,
    #[arg(long, conflicts_with = "operator_file")]
    pub operator_fd: Option<i32>,
    #[command(subcommand)]
    pub command: RunCommand,
}

#[derive(Debug, Subcommand)]
pub enum RunCommand {
    Start(LeafArgs),
    List(LeafArgs),
    Status(LeafArgs),
    Send(LeafArgs),
    Submit(LeafArgs),
    Wait(LeafArgs),
    Events(LeafArgs),
    Timeline(LeafArgs),
    Pending(LeafArgs),
    Respond(LeafArgs),
    Interrupt(LeafArgs),
    SetEffort(LeafArgs),
    AcquireWrite(LeafArgs),
    ReleaseWrite(LeafArgs),
    Pause(LeafArgs),
    Resume(LeafArgs),
    Recover(LeafArgs),
    Reconcile(LeafArgs),
    Fork(LeafArgs),
    CreateWriteContinuation(LeafArgs),
    Close(LeafArgs),
    Delete(LeafArgs),
    Verify(LeafArgs),
    Export(LeafArgs),
    Interaction {
        #[command(subcommand)]
        command: RunInteractionCommand,
    },
    Controller {
        #[command(subcommand)]
        command: RunControllerCommand,
    },
    Artifact {
        #[command(subcommand)]
        command: RunArtifactCommand,
    },
}
#[derive(Debug, Subcommand)]
pub enum RunInteractionCommand {
    Get(LeafArgs),
}
#[derive(Debug, Subcommand)]
pub enum RunControllerCommand {
    Reset(LeafArgs),
    Verify(LeafArgs),
}
#[derive(Debug, Subcommand)]
pub enum RunArtifactCommand {
    Show(LeafArgs),
    Read(LeafArgs),
    Export(LeafArgs),
}
impl RunCommand {
    const fn machine_name(&self) -> &'static str {
        match self {
            Self::Start(_) => "run.start",
            Self::List(_) => "run.list",
            Self::Status(_) => "run.status",
            Self::Send(_) => "run.send",
            Self::Submit(_) => "run.submit",
            Self::Wait(_) => "run.wait",
            Self::Events(_) => "run.events",
            Self::Timeline(_) => "run.timeline",
            Self::Pending(_) => "run.pending",
            Self::Respond(_) => "run.respond",
            Self::Interrupt(_) => "run.interrupt",
            Self::SetEffort(_) => "run.set_effort",
            Self::AcquireWrite(_) => "run.acquire_write",
            Self::ReleaseWrite(_) => "run.release_write",
            Self::Pause(_) => "run.pause",
            Self::Resume(_) => "run.resume",
            Self::Recover(_) => "run.recover",
            Self::Reconcile(_) => "run.reconcile",
            Self::Fork(_) => "run.fork",
            Self::CreateWriteContinuation(_) => "run.create_write_continuation",
            Self::Close(_) => "run.close",
            Self::Delete(_) => "run.delete",
            Self::Verify(_) => "run.verify",
            Self::Export(_) => "run.export",
            Self::Interaction { .. } => "run.interaction.get",
            Self::Controller { command } => match command {
                RunControllerCommand::Reset(_) => "run.controller.reset",
                RunControllerCommand::Verify(_) => "run.controller.verify",
            },
            Self::Artifact { command } => match command {
                RunArtifactCommand::Show(_) => "run.artifact.show",
                RunArtifactCommand::Read(_) => "run.artifact.read",
                RunArtifactCommand::Export(_) => "run.artifact.export",
            },
        }
    }
    fn leaf_args(&self) -> Option<&LeafArgs> {
        Some(match self {
            Self::Start(a)
            | Self::List(a)
            | Self::Status(a)
            | Self::Send(a)
            | Self::Submit(a)
            | Self::Wait(a)
            | Self::Events(a)
            | Self::Timeline(a)
            | Self::Pending(a)
            | Self::Respond(a)
            | Self::Interrupt(a)
            | Self::SetEffort(a)
            | Self::AcquireWrite(a)
            | Self::ReleaseWrite(a)
            | Self::Pause(a)
            | Self::Resume(a)
            | Self::Recover(a)
            | Self::Reconcile(a)
            | Self::Fork(a)
            | Self::CreateWriteContinuation(a)
            | Self::Close(a)
            | Self::Delete(a)
            | Self::Verify(a)
            | Self::Export(a) => a,
            Self::Interaction {
                command: RunInteractionCommand::Get(a),
            } => a,
            Self::Controller { command } => match command {
                RunControllerCommand::Reset(a) | RunControllerCommand::Verify(a) => a,
            },
            Self::Artifact { command } => match command {
                RunArtifactCommand::Show(a)
                | RunArtifactCommand::Read(a)
                | RunArtifactCommand::Export(a) => a,
            },
        })
    }
}

#[derive(Debug, Args)]
pub struct InitArgs {
    pub path: Option<PathBuf>,
    #[arg(long)]
    pub non_git: bool,
}

#[derive(Debug, Args)]
pub struct LeafArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
}

pub fn validate_argument_contract(command: &Command) -> Result<(), String> {
    let Some(leaf) = command.leaf_args() else {
        return Ok(());
    };
    let args = &leaf.args;
    for (left, right) in [
        ("--controller-file", "--controller-fd"),
        ("--operator-file", "--operator-fd"),
        ("--new-controller-file", "--new-controller-fd"),
        ("--message", "--instructions-file"),
        ("--instructions", "--instructions-file"),
        ("--instructions", "--instructions-stdin"),
        ("--instructions-file", "--instructions-stdin"),
    ] {
        if has(args, left) && has(args, right) {
            return Err(format!("{left} conflicts with {right}"));
        }
    }
    let parent_count = ["--parent-namespace", "--parent-kind", "--parent-id"]
        .into_iter()
        .filter(|flag| has(args, flag))
        .count();
    if parent_count != 0 && parent_count != 3 {
        return Err("parent namespace, kind, and id must be provided together".to_owned());
    }
    if has(args, "--leave-running") && !has(args, "--launch-probe") {
        return Err("--leave-running requires --launch-probe".to_owned());
    }
    let spec = leaf_spec(command.machine_name());
    validate_leaf_tokens(command.machine_name(), args, &spec)?;
    if let Command::Run(run) = command
        && matches!(command.machine_name(), "run.artifact.export" | "run.export")
    {
        let carrier_count = usize::from(run.controller_file.is_some())
            + usize::from(run.controller_fd.is_some())
            + usize::from(has(args, "--controller-file"))
            + usize::from(has(args, "--controller-fd"));
        if carrier_count != 1 {
            return Err("exactly one controller credential carrier is required".to_owned());
        }
    }
    Ok(())
}

fn has(args: &[OsString], flag: &str) -> bool {
    args.iter().any(|arg| {
        arg == OsStr::new(flag) || arg.to_string_lossy().starts_with(&format!("{flag}="))
    })
}

struct LeafSpec {
    values: &'static [&'static str],
    switches: &'static [&'static str],
    required: &'static [&'static str],
    positional: std::ops::RangeInclusive<usize>,
}

fn spec(
    values: &'static [&'static str],
    switches: &'static [&'static str],
    required: &'static [&'static str],
    min: usize,
    max: usize,
) -> LeafSpec {
    LeafSpec {
        values,
        switches,
        required,
        positional: min..=max,
    }
}

fn leaf_spec(command: &str) -> LeafSpec {
    const W: &[&str] = &["--workspace"];
    const C: &[&str] = &["--workspace", "--controller-file", "--controller-fd"];
    const O: &[&str] = &["--workspace", "--operator-file", "--operator-fd"];
    match command {
        "serve" => spec(&["--socket", "--ready-fd"], &[], &["--socket"], 0, 0),
        "engagement.call" => spec(
            &[
                "--workspace",
                "--request-fd",
                "--controller-file",
                "--controller-fd",
                "--new-controller-file",
                "--new-controller-fd",
            ],
            &[],
            &["--workspace", "--request-fd"],
            0,
            0,
        ),
        "controller.credential.create" => spec(
            &[
                "--kind",
                "--instance-id",
                "--subject-id",
                "--orchestration-policy",
                "--output",
            ],
            &[],
            &["--kind", "--instance-id", "--output"],
            0,
            0,
        ),
        "operator.credential.initialize" => spec(&["--output"], &[], &["--output"], 0, 0),
        "operator.credential.rotate" => spec(
            &["--operator-file", "--operator-fd", "--output"],
            &[],
            &["--output"],
            0,
            0,
        ),
        "workspace.inspect" | "workspace.writer.status" => spec(W, &[], &[], 0, 0),
        "workspace.writer.reset" => spec(
            &[
                "--workspace",
                "--operator-file",
                "--operator-fd",
                "--confirm-workspace-id",
            ],
            &["--require-worker-absence"],
            &["--confirm-workspace-id", "--require-worker-absence"],
            0,
            0,
        ),
        "workspace.writer.handoff_prepare" => spec(
            &[
                "--workspace",
                "--from",
                "--to",
                "--expected-generation",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &["--workspace", "--from", "--to", "--expected-generation"],
            0,
            0,
        ),
        "workspace.writer.handoff_commit" => spec(
            &[
                "--workspace",
                "--handoff-id",
                "--expected-generation",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &["--workspace", "--handoff-id", "--expected-generation"],
            0,
            0,
        ),
        "workspace.writer.handoff_cancel" => spec(
            &[
                "--workspace",
                "--handoff-id",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &["--workspace", "--handoff-id"],
            0,
            0,
        ),
        "profile.add" => spec(
            &["--workspace", "--codex-home", "--native-subagents", "--env"],
            &[],
            &["--codex-home", "--native-subagents"],
            2,
            usize::MAX,
        ),
        "profile.list" => spec(W, &[], &[], 0, 0),
        "profile.show" | "profile.remove" | "profile.membership.verify" => spec(W, &[], &[], 1, 1),
        "profile.doctor" => spec(W, &["--launch-probe", "--leave-running"], &[], 1, 1),
        "profile.server.status" | "profile.server.start" => spec(W, &[], &[], 1, 1),
        "profile.server.stop" | "profile.server.restart" => spec(O, &["--interrupt"], &[], 1, 1),
        "profile.server.migrate" => spec(
            &[
                "--workspace",
                "--operator-file",
                "--operator-fd",
                "--confirm-old-server-key",
                "--confirm-new-server-key",
            ],
            &["--interrupt"],
            &["--confirm-old-server-key", "--confirm-new-server-key"],
            1,
            1,
        ),
        "profile.membership.tombstone_orphan" => spec(
            &[
                "--workspace",
                "--operator-file",
                "--operator-fd",
                "--confirm-server-key",
                "--confirm-workspace-id",
                "--confirm-run-id",
            ],
            &[],
            &[
                "--confirm-server-key",
                "--confirm-workspace-id",
                "--confirm-run-id",
            ],
            1,
            1,
        ),
        "profile.state.reset" => spec(
            &[
                "--workspace",
                "--operator-file",
                "--operator-fd",
                "--confirm-server-key",
            ],
            &["--require-server-absence"],
            &["--confirm-server-key", "--require-server-absence"],
            1,
            1,
        ),
        "profile.diagnostics.list" => spec(
            &[
                "--workspace",
                "--after",
                "--limit",
                "--projection",
                "--operator-file",
                "--operator-fd",
            ],
            &[],
            &[],
            1,
            1,
        ),
        "profile.events" => spec(
            &[
                "--workspace",
                "--after",
                "--projection",
                "--operator-file",
                "--operator-fd",
            ],
            &["--follow"],
            &[],
            1,
            1,
        ),
        "specialist.policy.add" => spec(&["--workspace", "--file"], &[], &["--file"], 1, 1),
        "specialist.policy.list" => spec(W, &[], &[], 0, 0),
        "specialist.policy.show" | "specialist.policy.remove" => spec(W, &[], &[], 1, 1),
        "specialist.policy.validate" => spec(&["--workspace", "--file"], &[], &["--file"], 0, 0),
        "specialist.review" => spec(
            &[
                "--workspace",
                "--request-fd",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &["--workspace", "--request-fd"],
            0,
            0,
        ),
        "run.start" => spec(
            &[
                "--workspace",
                "--profile",
                "--control-mode",
                "--execution-lane",
                "--required-assurance",
                "--model",
                "--effort",
                "--purpose",
                "--purpose-label",
                "--parent-namespace",
                "--parent-kind",
                "--parent-id",
                "--require-capability",
                "--instructions",
                "--instructions-file",
                "--idempotency-key",
            ],
            &["--instructions-stdin"],
            &[
                "--workspace",
                "--profile",
                "--control-mode",
                "--execution-lane",
                "--required-assurance",
                "--purpose",
                "--idempotency-key",
            ],
            0,
            0,
        ),
        "run.list" => spec(W, &[], &[], 0, 0),
        "run.status"
        | "run.pending"
        | "run.interrupt"
        | "run.acquire_write"
        | "run.release_write"
        | "run.resume"
        | "run.recover"
        | "run.reconcile"
        | "run.close"
        | "run.verify"
        | "run.controller.verify" => spec(C, &[], &[], 1, 1),
        "run.send" | "run.submit" => spec(
            &[
                "--workspace",
                "--message",
                "--image",
                "--effort",
                "--idempotency-key",
                "--timeout",
                "--controller-file",
                "--controller-fd",
            ],
            &["--write"],
            &["--idempotency-key"],
            1,
            1,
        ),
        "run.wait" => spec(&["--workspace", "--timeout"], &[], &[], 2, 2),
        "run.events" => spec(
            &["--workspace", "--after", "--projection"],
            &["--follow"],
            &[],
            1,
            1,
        ),
        "run.timeline" => spec(
            &[
                "--workspace",
                "--after",
                "--limit",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &[],
            1,
            1,
        ),
        "run.interaction.get" => spec(C, &[], &[], 2, 2),
        "run.respond" => spec(
            &[
                "--workspace",
                "--request-id",
                "--idempotency-key",
                "--response-fd",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &["--request-id", "--idempotency-key"],
            1,
            1,
        ),
        "run.artifact.show" => spec(C, &[], &[], 2, 2),
        "run.artifact.read" => spec(
            &[
                "--workspace",
                "--offset",
                "--length",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &["--offset", "--length"],
            2,
            2,
        ),
        "run.artifact.export" => spec(
            &[
                "--workspace",
                "--output",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &["--output"],
            2,
            2,
        ),
        "run.set_effort" => spec(C, &[], &[], 2, 2),
        "run.pause" => spec(C, &["--interrupt"], &[], 1, 1),
        "run.fork" => spec(
            &[
                "--workspace",
                "--from",
                "--model",
                "--idempotency-key",
                "--controller-file",
                "--controller-fd",
            ],
            &["--fresh"],
            &["--from", "--idempotency-key"],
            0,
            0,
        ),
        "run.create_write_continuation" => spec(
            &[
                "--workspace",
                "--from",
                "--from-turn",
                "--reason",
                "--purpose",
                "--purpose-label",
                "--model",
                "--effort",
                "--required-assurance",
                "--require-capability",
                "--instructions-fd",
                "--handoff-summary-fd",
                "--artifact-ref",
                "--idempotency-key",
                "--new-controller-file",
                "--new-controller-fd",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &[
                "--from",
                "--from-turn",
                "--reason",
                "--purpose",
                "--idempotency-key",
            ],
            0,
            0,
        ),
        "run.delete" => spec(C, &["--confirm"], &["--confirm"], 1, 1),
        "run.export" => spec(
            &[
                "--workspace",
                "--output",
                "--controller-file",
                "--controller-fd",
            ],
            &[],
            &[],
            1,
            1,
        ),
        "run.controller.reset" => spec(
            &[
                "--workspace",
                "--confirm",
                "--operator-file",
                "--operator-fd",
                "--new-controller-file",
                "--new-controller-fd",
            ],
            &[],
            &["--confirm"],
            1,
            1,
        ),
        other => panic!("missing checked CLI leaf specification for {other}"),
    }
}

fn validate_leaf_tokens(command: &str, args: &[OsString], spec: &LeafSpec) -> Result<(), String> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut seen = BTreeSet::new();
    let mut values = BTreeMap::<String, Vec<String>>::new();
    let mut positionals = 0usize;
    let mut index = 0usize;
    let mut after_delimiter = false;
    while index < args.len() {
        let token = args[index].to_string_lossy();
        if after_delimiter {
            positionals += 1;
            index += 1;
            continue;
        }
        if token == "--" {
            after_delimiter = true;
            index += 1;
            continue;
        }
        if let Some((flag, value)) = token.split_once('=') {
            if !spec.values.contains(&flag) {
                return Err(format!("unknown option {flag}"));
            }
            seen.insert(flag.to_owned());
            values
                .entry(flag.to_owned())
                .or_default()
                .push(value.to_owned());
            index += 1;
            continue;
        }
        if token.starts_with("--") {
            if spec.switches.contains(&token.as_ref()) {
                seen.insert(token.into_owned());
                index += 1;
                continue;
            }
            if spec.values.contains(&token.as_ref()) {
                if index + 1 >= args.len() || args[index + 1].to_string_lossy().starts_with("--") {
                    return Err(format!("{token} requires a value"));
                }
                seen.insert(token.into_owned());
                let flag = args[index].to_string_lossy().into_owned();
                values
                    .entry(flag)
                    .or_default()
                    .push(args[index + 1].to_string_lossy().into_owned());
                index += 2;
                continue;
            }
            return Err(format!("unknown option {token}"));
        }
        positionals += 1;
        index += 1;
    }
    for required in spec.required {
        if !seen.contains(*required) {
            return Err(format!("missing required option {required}"));
        }
    }
    const REPEATABLE: &[&str] = &["--env", "--image", "--require-capability", "--artifact-ref"];
    for (flag, supplied) in &values {
        if supplied.len() > 1 && !REPEATABLE.contains(&flag.as_str()) {
            return Err(format!("option {flag} cannot be repeated"));
        }
        for value in supplied {
            validate_option_value(flag, value)?;
        }
    }
    if !spec.positional.contains(&positionals) {
        return Err(format!("invalid positional argument count {positionals}"));
    }
    for group in [
        ["--controller-file", "--controller-fd"],
        ["--operator-file", "--operator-fd"],
        ["--new-controller-file", "--new-controller-fd"],
    ] {
        if group.iter().filter(|flag| seen.contains(**flag)).count() > 1 {
            return Err(format!("{} conflicts with {}", group[0], group[1]));
        }
    }
    if let Some(group) = match command {
        "engagement.call" | "specialist.review" => Some(["--controller-file", "--controller-fd"]),
        "run.create_write_continuation" => Some(["--new-controller-file", "--new-controller-fd"]),
        _ => None,
    } {
        require_exactly_one(&seen, group)?;
    }
    if command == "profile.add" && !args.iter().any(|arg| arg == "--") {
        return Err("profile add requires -- before the executable argv".to_owned());
    }
    Ok(())
}

fn require_exactly_one(
    seen: &std::collections::BTreeSet<String>,
    group: [&str; 2],
) -> Result<(), String> {
    if group.iter().filter(|flag| seen.contains(**flag)).count() == 1 {
        Ok(())
    } else {
        Err(format!(
            "exactly one of {} or {} is required",
            group[0], group[1]
        ))
    }
}

fn validate_option_value(flag: &str, value: &str) -> Result<(), String> {
    if flag.ends_with("-fd") {
        let fd = value
            .parse::<i32>()
            .map_err(|_| format!("{flag} requires an integer file descriptor"))?;
        if fd < 0 {
            return Err(format!("{flag} requires a nonnegative file descriptor"));
        }
    }
    if ["--limit", "--offset", "--length", "--expected-generation"].contains(&flag) {
        value
            .parse::<u64>()
            .map_err(|_| format!("{flag} requires an unsigned integer"))?;
    }
    let allowed: Option<&[&str]> = match flag {
        "--control-mode" => Some(&["direct-interactive", "managed-agent"]),
        "--execution-lane" => Some(&["shared-readonly", "dedicated"]),
        "--required-assurance" => Some(&[
            "best-effort-personal-alpha",
            "verified-thread-scoped-control",
            "strong-process-containment",
        ]),
        "--projection" => Some(&["minimal", "operational"]),
        "--native-subagents" => Some(&["enabled"]),
        "--reason" => Some(&[
            "shared-readonly-source",
            "access-transition-unavailable",
            "access-transition-unverified",
        ]),
        "--kind" => Some(&[
            "human-cli",
            "interactive-client",
            "workflow-orchestrator",
            "automation",
            "other",
        ]),
        "--purpose" => Some(&[
            "interactive",
            "planning",
            "implementation",
            "review",
            "research",
            "discussion",
            "workflow-stage",
            "other",
        ]),
        _ => None,
    };
    if let Some(allowed) = allowed
        && !allowed.contains(&value)
    {
        return Err(format!("invalid value for {flag}: {value}"));
    }
    if flag == "--image"
        && !["auto=", "low=", "high="]
            .iter()
            .any(|prefix| value.starts_with(prefix) && value.len() > prefix.len())
    {
        return Err("--image requires auto|low|high=<path>".to_owned());
    }
    if ["--socket", "--codex-home"].contains(&flag) && !std::path::Path::new(value).is_absolute() {
        return Err(format!("{flag} requires an absolute path"));
    }
    if flag == "--timeout" && !valid_duration(value) {
        return Err("--timeout requires a positive duration such as 500ms, 30s, or 2m".to_owned());
    }
    Ok(())
}

fn valid_duration(value: &str) -> bool {
    for suffix in ["ms", "s", "m", "h"] {
        if let Some(number) = value.strip_suffix(suffix) {
            return number.parse::<u64>().is_ok_and(|number| number > 0);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_capabilities_parses() {
        assert!(Cli::try_parse_from(["dolgorae", "runtime", "capabilities"]).is_ok());
    }
    #[test]
    fn unknown_nested_command_is_rejected() {
        assert!(Cli::try_parse_from(["dolgorae", "run", "bogus"]).is_err());
    }
    #[test]
    fn exact_nested_command_identity_is_preserved() {
        let cli = Cli::try_parse_from(["dolgorae", "operator", "credential", "rotate"]).unwrap();
        assert_eq!(cli.command.machine_name(), "operator.credential.rotate");
        let cli = Cli::try_parse_from(["dolgorae", "run", "start"]).unwrap();
        assert_eq!(cli.command.machine_name(), "run.start");
    }
    #[test]
    fn checked_argument_conflicts_are_rejected() {
        let args = [
            "dolgorae",
            "run",
            "start",
            "--controller-file",
            "a",
            "--controller-fd",
            "3",
        ]
        .map(OsString::from);
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(validate_argument_contract(&cli.command).is_err());
        let args = ["dolgorae", "run", "start", "--parent-id", "x"].map(OsString::from);
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(validate_argument_contract(&cli.command).is_err());
    }

    #[test]
    fn required_and_unknown_leaf_arguments_are_rejected() {
        let cli = Cli::try_parse_from(["dolgorae", "run", "start"]).unwrap();
        assert!(validate_argument_contract(&cli.command).is_err());
        let cli = Cli::try_parse_from(["dolgorae", "profile", "list", "--bogus"]).unwrap();
        assert!(validate_argument_contract(&cli.command).is_err());
    }
}
