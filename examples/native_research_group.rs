//! Opt-in accountless research-group driver; runtime evidence is emitted only by execution.
#![deny(unsafe_code)]
#![allow(unexpected_cfgs)]
#[cfg(all(test, dolgorae_native_acceptance))]
#[allow(dead_code, unused_imports, clippy::enum_variant_names)]
#[path = "../src/lib.rs"]
mod product;
#[cfg(all(test, dolgorae_native_acceptance))]
#[allow(unused_imports)]
use product::*;
#[cfg(all(test, dolgorae_native_acceptance))]
#[path = "support/native_research_group.rs"]
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
            std::env::var("DOLGORAE_RUN_NATIVE_RESEARCH_GROUP").as_deref(),
            Ok("1")
        );
        if args.len() == 3 && args[1] == "--cleanup" {
            println!("{}", driver::cleanup(std::path::Path::new(&args[2])));
            return;
        }
        if args.len() == 5 && args[1] == "--recover-original-task" {
            println!(
                "{}",
                driver::recover_original_task(
                    std::path::Path::new(&args[2]),
                    args[3].to_str().unwrap().parse().unwrap(),
                    args[4].to_str().unwrap().parse().unwrap()
                )
            );
            return;
        }
        assert_eq!(args.len(), 4);
        println!(
            "{}",
            driver::campaign(
                std::path::Path::new(&args[1]),
                std::path::Path::new(&args[2]),
                args[3].to_str().unwrap().parse().unwrap()
            )
        );
    }
}
#[cfg(not(all(test, dolgorae_native_acceptance)))]
fn main() {
    eprintln!("Build only for the opted-in native research-group campaign");
    std::process::exit(2);
}
