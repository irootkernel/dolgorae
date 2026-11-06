//! Isolated TASK-025 probe, intentionally outside the product command surface.
use clap::Parser;
use std::process::ExitCode;

#[path = "support/live_transport.rs"]
pub mod live_transport;
#[path = "support/live_transport_mcp.rs"]
pub mod live_transport_mcp;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    session_id: String,
    #[arg(long)]
    run_id: String,
    #[arg(long, default_value_t = 1)]
    generation: u64,
    #[arg(long)]
    dedicated_lane: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match live_transport_mcp::ProbeProcessBinding::parse(
        &args.session_id,
        &args.run_id,
        args.generation,
        args.dedicated_lane,
    )
    .and_then(live_transport_mcp::serve_stdio)
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Dolgorae transport probe failed: {}", error.code);
            ExitCode::from(error.exit_status())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_binding_is_separate_from_tool_arguments() {
        let args = Args::try_parse_from([
            "live_transport_probe",
            "--session-id",
            "018f0000-0000-7000-8000-000000000001",
            "--run-id",
            "018f0000-0000-7000-8000-000000000002",
            "--generation",
            "3",
            "--dedicated-lane",
        ])
        .unwrap();
        let binding = live_transport_mcp::ProbeProcessBinding::parse(
            &args.session_id,
            &args.run_id,
            args.generation,
            args.dedicated_lane,
        )
        .unwrap();
        assert_eq!(binding.generation, 3);
        assert_eq!(
            binding.lane,
            live_transport::ExecutionLaneKind::DedicatedLane
        );
        assert!(
            Args::try_parse_from([
                "live_transport_probe",
                "--session-id",
                "018f0000-0000-7000-8000-000000000001",
                "--run-id",
                "018f0000-0000-7000-8000-000000000002",
                "--source-turn-id",
                "turn-forged",
            ])
            .is_err()
        );
    }
}
