//! Fixture-only preparation for the exact production native-host scenarios.
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
#[path = "support/native_production_host.rs"]
mod driver;
#[cfg(all(test, dolgorae_native_acceptance))]
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == "__trusted-specialist-host")
    {
        assert_eq!(
            std::env::var("DOLGORAE_RUN_NATIVE_PRODUCTION_HOST").as_deref(),
            Ok("1")
        );
        if let Err(error) = specialist_stdio::run(&args[2..]) {
            eprintln!("{}", trusted_host::administrative_error_code(&error));
            std::process::exit(4);
        }
    } else if args.len() == 4 && args[1] == "__worker" && args[2] == "--bootstrap" {
        if let Err(error) = worker::run_hidden_worker(std::path::Path::new(&args[3])) {
            let _ = worker::write_startup_handoff(&worker::StartupHandoff::Failed {
                code: error.code().into(),
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
            std::env::var("DOLGORAE_RUN_NATIVE_PRODUCTION_HOST").as_deref(),
            Ok("1")
        );
        assert_eq!(args.len(), 3);
        let root = std::path::Path::new(&args[2]);
        let output = match args[1].to_str().unwrap() {
            "--prepare" => driver::prepare(root),
            "--task085-prepare" => driver::task085_prepare(root),
            "--task085-control-plane" => driver::task085_control_plane(root),
            "--task085-run-managed" => driver::task085_run_managed(root),
            "--task085-observe" => driver::task085_observe(root),
            "--task086-control-plane" => driver::task086_control_plane(root),
            "--task086-unknown" => driver::task086_unknown(root),
            "--task086-merged" => driver::task086_merged(root),
            "--task086-finish" => driver::task086_finish(root),
            "--assign" => driver::assign(root),
            "--observe" => driver::observe(root),
            "--ordinary-read-result" => driver::ordinary_read_result(root),
            "--ordinary-read-merge" => driver::ordinary_read_merge(root),
            "--ordinary-synthesis-input" => driver::ordinary_synthesis_input(root),
            "--ordinary-managed-read-result" => driver::ordinary_managed_read_result(root),
            "--cleanup" => driver::cleanup(root),
            "--peer-read" => driver::peer_read(root),
            _ => panic!("unsupported production fixture operation"),
        };
        println!("{}", output);
    }
}
#[cfg(not(all(test, dolgorae_native_acceptance)))]
fn main() {
    eprintln!("Build through the opted-in production host driver instructions");
    std::process::exit(2);
}
