//! Dolgorae v2 CLI and sole composition root wiring drivers into core/API/MCP.

mod commands;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use dolgorae_api::UdsClient;
use dolgorae_core::{AccessMode, ApprovalPolicy, CORE_VERSION, WaitMode};
use dolgorae_spi::AgentDriver;
use driver_agy_acp::AgyAcpDriver;
use driver_claude_wire::ClaudeWireDriver;
use driver_codex_app::CodexAppDriver;

#[derive(Debug, Parser)]
#[command(
    name = "dolgorae",
    version = CORE_VERSION,
    about = "Dolgorae v2 multi-provider actor control layer"
)]
struct Cli {
    /// Explicit Dolgorae home directory (defaults to DOLGORAE_HOME or ~/.dolgorae).
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    /// Explicit Unix domain socket path (defaults to DOLGORAE_SOCKET or <home>/run/dolgorae.sock).
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliAccessMode {
    ReadOnly,
    WorkspaceWrite,
}

impl From<CliAccessMode> for AccessMode {
    fn from(v: CliAccessMode) -> Self {
        match v {
            CliAccessMode::ReadOnly => Self::ReadOnly,
            CliAccessMode::WorkspaceWrite => Self::WorkspaceWrite,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliApprovalPolicy {
    Interactive,
    AutoDeny,
    AutoApproveScoped,
}

impl From<CliApprovalPolicy> for ApprovalPolicy {
    fn from(v: CliApprovalPolicy) -> Self {
        match v {
            CliApprovalPolicy::Interactive => Self::Interactive,
            CliApprovalPolicy::AutoDeny => Self::AutoDeny,
            CliApprovalPolicy::AutoApproveScoped => Self::AutoApproveScoped,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliWaitMode {
    Any,
    All,
}

impl From<CliWaitMode> for WaitMode {
    fn from(v: CliWaitMode) -> Self {
        match v {
            CliWaitMode::Any => Self::Any,
            CliWaitMode::All => Self::All,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print Dolgorae version information.
    Version,
    /// Start the local UDS HTTP/1.1 JSON+SSE daemon.
    Serve,
    /// Run the stdio MCP server (External Host Mode or --scoped Actor Mode).
    Mcp {
        /// Run in Scoped Actor Mode using DOLGORAE_ACTOR_TOKEN.
        #[arg(long)]
        scoped: bool,
    },
    /// Probe registered runtime drivers (alias for `driver list`).
    Drivers,
    /// Inspect runtime drivers.
    Driver {
        #[command(subcommand)]
        action: DriverCmd,
    },
    /// Inspect resolved Role / Persona definitions.
    Role {
        #[command(subcommand)]
        action: RoleCmd,
    },
    /// Operate unified sessions and delegated actors.
    Session {
        #[command(subcommand)]
        action: SessionCmd,
    },
    /// Manage optional workspace single-writer leases.
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCmd,
    },
    /// Read immutable content-addressed artifacts.
    Artifact {
        #[command(subcommand)]
        action: ArtifactCmd,
    },
}

#[derive(Debug, Subcommand)]
enum DriverCmd {
    List,
    Doctor,
}

#[derive(Debug, Subcommand)]
enum RoleCmd {
    List {
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Show {
        name: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum SessionCmd {
    Start {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        driver: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long, value_enum)]
        access: Option<CliAccessMode>,
        #[arg(long, value_enum)]
        approval_policy: Option<CliApprovalPolicy>,
        #[arg(long)]
        output_schema: Option<String>,
        #[arg(long)]
        allow_delegation: bool,
        #[arg(long)]
        max_delegation_depth: Option<u8>,
        #[arg(long)]
        allow_actor_collaboration: Option<bool>,
    },
    List {
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        root: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    Get {
        session_id: String,
    },
    Resume {
        session_id: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long, value_enum)]
        access: Option<CliAccessMode>,
        #[arg(long, value_enum)]
        approval_policy: Option<CliApprovalPolicy>,
    },
    Close {
        session_id: String,
    },
    Wait {
        #[arg(required = true)]
        session_ids: Vec<String>,
        #[arg(long, value_enum, default_value = "all")]
        mode: CliWaitMode,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
        #[arg(long)]
        waiter: Option<String>,
    },
    Turn {
        session_id: String,
        text: String,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        output_schema: Option<String>,
    },
    Interrupt {
        session_id: String,
        turn_id: String,
    },
    Timeline {
        session_id: String,
        #[arg(long)]
        page_token: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Events {
        session_id: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        max_events: Option<usize>,
    },
    Interactions {
        session_id: String,
        #[arg(long)]
        all: bool,
    },
    Answer {
        session_id: String,
        interaction_id: String,
        #[arg(long)]
        turn_id: String,
        #[arg(long)]
        session_generation: u64,
        #[arg(long)]
        expected_interaction_revision: u64,
        #[arg(long)]
        expected_session_revision: u64,
        #[arg(long)]
        decision_id: String,
        #[arg(long)]
        payload: Option<String>,
    },
    MailboxSend {
        session_id: String,
        body: String,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        from: Option<String>,
    },
    MailboxRead {
        session_id: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}

#[derive(Debug, Subcommand)]
enum WorkspaceCmd {
    Lease {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        session_id: String,
    },
    Transfer {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    Release {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        session_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum ArtifactCmd {
    Read {
        artifact_id: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 262_144)]
        max_bytes: usize,
    },
}

fn registered_drivers() -> Vec<Arc<dyn AgentDriver>> {
    vec![
        Arc::new(CodexAppDriver::new()),
        Arc::new(ClaudeWireDriver::new()),
        Arc::new(AgyAcpDriver::new()),
    ]
}

fn init_stderr_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

#[tokio::main]
async fn main() {
    init_stderr_tracing();
    let cli = Cli::parse();

    if let Err(err) = dispatch(cli).await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn dispatch(cli: Cli) -> Result<(), String> {
    if matches!(cli.command, Command::Version) {
        commands::run_version(cli.json);
        return Ok(());
    }

    let home_dir = commands::resolve_home_dir(cli.home)?;
    let socket_path = commands::resolve_socket_path(cli.socket, &home_dir);
    let client = UdsClient::new(socket_path.clone());

    match cli.command {
        Command::Version => unreachable!(),
        Command::Serve => commands::run_serve(home_dir, socket_path, registered_drivers()).await,
        Command::Mcp { scoped } => commands::run_mcp(scoped, socket_path).await,
        Command::Drivers
        | Command::Driver {
            action: DriverCmd::List | DriverCmd::Doctor,
        } => commands::run_driver_list(cli.json, &socket_path, registered_drivers()).await,
        Command::Role { action } => match action {
            RoleCmd::List { workspace } => {
                commands::run_role_list(cli.json, &client, workspace).await
            }
            RoleCmd::Show { name, workspace } => {
                commands::run_role_show(cli.json, &client, &name, workspace).await
            }
        },
        Command::Session { action } => match action {
            SessionCmd::Start {
                workspace,
                driver,
                role,
                parent,
                model,
                effort,
                access,
                approval_policy,
                output_schema,
                allow_delegation,
                max_delegation_depth,
                allow_actor_collaboration,
            } => {
                commands::run_session_start(
                    cli.json,
                    &client,
                    workspace,
                    driver,
                    role,
                    parent,
                    model,
                    effort,
                    access.map(Into::into),
                    approval_policy.map(Into::into),
                    output_schema,
                    allow_delegation,
                    max_delegation_depth,
                    allow_actor_collaboration,
                )
                .await
            }
            SessionCmd::List {
                parent,
                root,
                status,
            } => commands::run_session_list(cli.json, &client, parent, root, status).await,
            SessionCmd::Get { session_id } => {
                commands::run_session_get(cli.json, &client, &session_id).await
            }
            SessionCmd::Resume {
                session_id,
                model,
                effort,
                access,
                approval_policy,
            } => {
                commands::run_session_resume(
                    cli.json,
                    &client,
                    &session_id,
                    model,
                    effort,
                    access.map(Into::into),
                    approval_policy.map(Into::into),
                )
                .await
            }
            SessionCmd::Close { session_id } => {
                commands::run_session_close(cli.json, &client, &session_id).await
            }
            SessionCmd::Wait {
                session_ids,
                mode,
                timeout_ms,
                waiter,
            } => {
                commands::run_session_wait(
                    cli.json,
                    &client,
                    session_ids,
                    mode.into(),
                    timeout_ms,
                    waiter,
                )
                .await
            }
            SessionCmd::Turn {
                session_id,
                text,
                effort,
                output_schema,
            } => {
                commands::run_session_turn(
                    cli.json,
                    &client,
                    &session_id,
                    text,
                    effort,
                    output_schema,
                )
                .await
            }
            SessionCmd::Interrupt {
                session_id,
                turn_id,
            } => commands::run_session_interrupt(cli.json, &client, &session_id, &turn_id).await,
            SessionCmd::Timeline {
                session_id,
                page_token,
                limit,
            } => {
                commands::run_session_timeline(cli.json, &client, &session_id, page_token, limit)
                    .await
            }
            SessionCmd::Events {
                session_id,
                after,
                max_events,
            } => {
                commands::run_session_events(cli.json, &client, &session_id, after, max_events)
                    .await
            }
            SessionCmd::Interactions { session_id, all } => {
                commands::run_session_interactions(cli.json, &client, &session_id, all).await
            }
            SessionCmd::Answer {
                session_id,
                interaction_id,
                turn_id,
                session_generation,
                expected_interaction_revision,
                expected_session_revision,
                decision_id,
                payload,
            } => {
                commands::run_session_answer(
                    cli.json,
                    &client,
                    &session_id,
                    &interaction_id,
                    turn_id,
                    session_generation,
                    expected_interaction_revision,
                    expected_session_revision,
                    decision_id,
                    payload,
                )
                .await
            }
            SessionCmd::MailboxSend {
                session_id,
                body,
                subject,
                from,
            } => {
                commands::run_mailbox_send(cli.json, &client, &session_id, body, subject, from)
                    .await
            }
            SessionCmd::MailboxRead {
                session_id,
                after,
                limit,
            } => commands::run_mailbox_read(cli.json, &client, &session_id, after, limit).await,
        },
        Command::Workspace { action } => match action {
            WorkspaceCmd::Lease {
                workspace,
                session_id,
            } => commands::run_workspace_lease(cli.json, &client, workspace, session_id).await,
            WorkspaceCmd::Transfer {
                workspace,
                from,
                to,
            } => commands::run_workspace_transfer(cli.json, &client, workspace, from, to).await,
            WorkspaceCmd::Release {
                workspace,
                session_id,
            } => commands::run_workspace_release(cli.json, &client, workspace, session_id).await,
        },
        Command::Artifact { action } => match action {
            ArtifactCmd::Read {
                artifact_id,
                offset,
                max_bytes,
            } => {
                commands::run_artifact_read(cli.json, &client, &artifact_id, offset, max_bytes)
                    .await
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_root_registers_all_tier1_drivers() {
        let drivers = registered_drivers();
        let ids: Vec<&str> = drivers.iter().map(|d| d.driver_id()).collect();
        assert_eq!(ids, vec!["codex-app", "claude-wire", "agy-acp"]);
    }
}
