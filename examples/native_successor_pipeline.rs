//! Explicitly opted-in native successor acceptance; no product transport is added.
#![deny(unsafe_code)]
#![allow(unexpected_cfgs)]

// The sealed caller-scope fixture remains test-only. Compile the same production
// modules in this test driver rather than exporting a provenance constructor.
#[cfg(all(test, dolgorae_native_acceptance))]
#[allow(dead_code, unused_imports)]
#[allow(
    clippy::enum_variant_names,
    reason = "embedding public production types in a private test module changes only lint visibility"
)]
#[path = "../src/lib.rs"]
mod product;
#[cfg(all(test, dolgorae_native_acceptance))]
#[allow(unused_imports)]
use product::*;

#[cfg(all(test, dolgorae_native_acceptance))]
#[path = "support/native_successor_pipeline.rs"]
mod driver;

#[cfg(all(test, dolgorae_native_acceptance))]
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() == 4 && args[1] == "__worker" && args[2] == "--bootstrap" {
        if let Err(error) = worker::run_hidden_worker(std::path::Path::new(&args[3])) {
            let _ = worker::write_startup_handoff(&worker::StartupHandoff::Failed {
                code: error.code().to_owned(),
            });
            std::process::exit(machine::exit_status_for(error.code()).into());
        }
    } else if args.len() == 4
        && args[1] == profile::PROFILE_LOG_DRAINER_COMMAND
        && args[2] == "--root"
    {
        profile::run_log_drainer(std::path::Path::new(&args[3])).unwrap();
    } else {
        assert_eq!(
            std::env::var("DOLGORAE_RUN_NATIVE_INTEGRATED_ACCEPTANCE").as_deref(),
            Ok("1")
        );
        assert_eq!(args.len(), 3);
        if args[1] == "--cleanup" {
            assert!(driver::cleanup(std::path::Path::new(&args[2])));
            return;
        }
        assert!(
            !(std::env::var("DOLGORAE_NATIVE_SUCCESSOR_STARTUP_COMPONENT").as_deref() == Ok("1")
                && std::env::var("DOLGORAE_NATIVE_SCHEDULER_COMPONENT").as_deref() == Ok("1")),
            "successor startup and scheduler components are mutually exclusive"
        );
        println!(
            "{}",
            if std::env::var("DOLGORAE_NATIVE_COLLABORATION_COMPONENT").as_deref() == Ok("1") {
                driver::collaboration_component(
                    std::path::Path::new(&args[1]),
                    std::path::Path::new(&args[2]),
                )
            } else if std::env::var("DOLGORAE_NATIVE_SCHEDULER_COMPONENT").as_deref() == Ok("1") {
                driver::scheduler_component(
                    std::path::Path::new(&args[1]),
                    std::path::Path::new(&args[2]),
                )
            } else {
                driver::campaign(
                    std::path::Path::new(&args[1]),
                    std::path::Path::new(&args[2]),
                )
            }
        );
    }
}

#[cfg(not(all(test, dolgorae_native_acceptance)))]
fn main() {
    eprintln!("Build this driver only through the opted-in integrated acceptance harness");
    std::process::exit(2);
}
