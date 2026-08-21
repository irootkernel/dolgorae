use serde_json::Value;
use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dolgorae"))
}

#[test]
fn help_and_version_are_machine_envelopes() {
    for flag in ["--help", "--version"] {
        let output = binary().arg(flag).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(output.stdout.ends_with(b"\n"));
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["ok"], true);
        assert_eq!(envelope.as_object().unwrap().len(), 5);
    }
}

#[test]
fn unknown_command_is_structured_exit_two() {
    let output = binary().arg("definitely-unknown").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "unknown");
    assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn human_boundary_does_not_emit_json() {
    let output = binary().args(["--human", "--version"]).output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("dolgorae ")
    );
}

#[test]
fn runtime_capabilities_contains_discovery_flag() {
    let output = binary().args(["runtime", "capabilities"]).output().unwrap();
    assert!(output.status.success());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "runtime.capabilities");
    assert_eq!(
        envelope["data"]["features"]["brokered_independent_subagent_runs"],
        false
    );
}
