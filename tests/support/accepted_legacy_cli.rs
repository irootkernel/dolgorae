//! Test-only historical adapter. It is never a product binary or feature.
#![allow(dead_code)]
#![deny(unsafe_code)]

#[path = "../../src/lib.rs"]
// The ordinary unit test functions are omitted by this custom harness, leaving
// their module imports unused. Product builds retain their normal lint policy.
#[allow(unused_imports)]
mod product;
pub(crate) use product::codex_socket;
pub use product::*;
#[path = "accepted_legacy_fixture.rs"]
mod fixture;

use clap::Parser as _;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.len() == 4 && args[1] == profile::PROFILE_LOG_DRAINER_COMMAND && args[2] == "--root" {
        return match profile::run_log_drainer(Path::new(&args[3])) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => ExitCode::from(error.exit_status()),
        };
    }
    if let Ok(cli) = cli::Cli::try_parse_from(&args) {
        match cli.command {
            cli::Command::Worker(args) => {
                return match worker::run_hidden_worker(&args.bootstrap) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        let code = error.code();
                        let _ = worker::write_startup_handoff(&worker::StartupHandoff::Failed {
                            code: code.to_owned(),
                        });
                        ExitCode::from(machine::exit_status_for(code))
                    }
                };
            }
            cli::Command::SpecialistReviewMcp(args) => {
                return match mcp_review_server::serve_stdio(&args.workspace, &args.profile) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => ExitCode::from(error.exit_status()),
                };
            }
            cli::Command::LiveTransportMcp(args) => {
                return match live_transport_mcp::ProbeProcessBinding::parse(
                    &args.session_id,
                    &args.run_id,
                    args.generation,
                    args.dedicated_lane,
                )
                .and_then(live_transport_mcp::serve_stdio)
                {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => ExitCode::from(error.exit_status()),
                };
            }
            _ => {}
        }
    }
    fixture::run_fixture()
}
