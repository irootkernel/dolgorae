//! Dolgorae v2 CLI and sole composition root wiring drivers into core/API/MCP.

use std::sync::Arc;

use clap::{Parser, Subcommand};
use dolgorae_api::API_VERSION;
use dolgorae_core::CORE_VERSION;
use dolgorae_mcp::McpMode;
use dolgorae_spi::AgentDriver;
use driver_agy_acp::AgyAcpDriver;
use driver_claude_wire::ClaudeWireDriver;
use driver_codex_app::CodexAppDriver;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "dolgorae", version = CORE_VERSION, about = "Dolgorae v2 multi-provider actor control layer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print Dolgorae version information.
    Version {
        /// Output machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Probe registered runtime drivers.
    Drivers,
    /// Start the local UDS HTTP/1.1 JSON+SSE daemon.
    Serve,
    /// Run the stdio MCP server.
    Mcp {
        /// Run in Scoped Actor Mode for an injected child agent.
        #[arg(long)]
        scoped: bool,
    },
}

#[derive(Serialize)]
struct VersionJson<'a> {
    name: &'a str,
    version: String,
    api_version: &'a str,
}

fn registered_drivers() -> Vec<Arc<dyn AgentDriver>> {
    vec![
        Arc::new(CodexAppDriver::new()),
        Arc::new(ClaudeWireDriver::new()),
        Arc::new(AgyAcpDriver::new()),
    ]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Version { json } => {
            if json {
                let payload = VersionJson {
                    name: "dolgorae",
                    version: format!("v{CORE_VERSION}"),
                    api_version: API_VERSION,
                };
                println!("{}", serde_json::to_string(&payload)?);
            } else {
                println!("dolgorae v{CORE_VERSION}");
            }
        }
        Command::Drivers => {
            let mut results = Vec::new();
            for driver in registered_drivers() {
                results.push(driver.probe().await?);
            }
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
        Command::Serve => {
            eprintln!(
                "dolgorae serve ({API_VERSION}) implementation is scheduled for V2-T02/V2-T04"
            );
            std::process::exit(2);
        }
        Command::Mcp { scoped } => {
            let mode = if scoped {
                McpMode::ScopedActor
            } else {
                McpMode::ExternalHost
            };
            eprintln!("dolgorae mcp ({mode:?}) implementation is scheduled for V2-T04");
            std::process::exit(2);
        }
    }
    Ok(())
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
