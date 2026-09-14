use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources(directory: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn direct_crate_dependencies(source: &str) -> BTreeSet<String> {
    source
        .match_indices("crate::")
        .filter_map(|(index, _)| {
            let suffix = &source[index + "crate::".len()..];
            let module = suffix
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect::<String>();
            (!module.is_empty()).then_some(module)
        })
        .collect()
}

#[test]
fn source_module_dependencies_match_the_approved_graph() {
    let approved = BTreeMap::from([
        ("app_server", &["jcs"][..]),
        ("audit", &["event", "jcs"][..]),
        ("interaction", &["domain", "machine", "snapshot"][..]),
        (
            "interaction_payload",
            &["domain", "interaction", "machine", "workspace"][..],
        ),
        ("cli", &[][..]),
        (
            "conformance",
            &[
                "audit",
                "domain",
                "event",
                "fault",
                "jcs",
                "ledger",
                "machine",
                "workspace",
            ][..],
        ),
        (
            "controller",
            &[
                "darwin",
                "domain",
                "jcs",
                "ledger",
                "machine",
                "paths",
                "projection",
                "run",
                "workspace",
            ][..],
        ),
        ("darwin", &["providers"][..]),
        ("domain", &[][..]),
        (
            "engagement",
            &[
                "controller",
                "domain",
                "jcs",
                "machine",
                "run",
                "specialist",
                "task_request",
                "workspace",
            ][..],
        ),
        (
            "external_engagement",
            &[
                "controller",
                "cli",
                "darwin",
                "domain",
                "engagement",
                "jcs",
                "machine",
                "paths",
                "run",
                "semantic",
                "turn",
                "worker",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "event",
            &["audit", "domain", "jcs", "projection", "workspace"][..],
        ),
        ("fault", &[][..]),
        (
            "global_profile",
            &["darwin", "jcs", "machine", "paths", "profile", "workspace"][..],
        ),
        (
            "global_runtime",
            &[
                "darwin",
                "global_profile",
                "jcs",
                "machine",
                "paths",
                "profile",
                "workspace",
            ][..],
        ),
        ("jcs", &[][..]),
        (
            "ledger",
            &[
                "audit",
                "darwin",
                "event",
                "fault",
                "jcs",
                "machine",
                "projection",
                "workspace",
            ][..],
        ),
        ("machine", &["paths", "workspace"][..]),
        (
            "mutation_admission",
            &["audit", "domain", "fault", "jcs", "ledger", "machine"][..],
        ),
        (
            "orchestration",
            &[
                "darwin",
                "domain",
                "engagement",
                "jcs",
                "machine",
                "run",
                "specialist_policy",
                "workspace",
            ][..],
        ),
        ("mcp_review", &[][..]),
        (
            "mcp_review_server",
            &["machine", "mcp_review", "semantic"][..],
        ),
        (
            "profile",
            &[
                "app_server",
                "controller",
                "darwin",
                "global_profile",
                "global_runtime",
                "jcs",
                "machine",
                "paths",
                "workspace",
            ][..],
        ),
        ("projection", &["audit", "domain", "jcs"][..]),
        ("protocol", &[][..]),
        ("providers", &[][..]),
        ("paths", &[][..]),
        // The one-shot review coordinator is the product composition boundary:
        // it binds trusted adapter state, the durable engagement, and the
        // ordinary semantic Run service without moving product logic into main.
        (
            "review",
            &[
                "cli",
                "controller",
                "domain",
                "engagement",
                "machine",
                "profile",
                "review_target",
                "run",
                "semantic",
                "specialist",
                "workspace",
            ][..],
        ),
        // The immutable target coordinator owns source capture and settlement
        // while delegating canonical workspace discovery and Machine errors to
        // their existing authorities.
        (
            "review_target",
            &["cli", "machine", "paths", "workspace"][..],
        ),
        // `run` names `projection` because the Run record and the Run's
        // durable state projection are two halves of the same durable state: an
        // observer that may not take the ledger still has to read the
        // projection the ledger commits beside the manifest.
        (
            "run",
            &[
                "audit",
                "darwin",
                "domain",
                "global_runtime",
                "jcs",
                "machine",
                "projection",
                "workspace",
            ][..],
        ),
        ("runtime", &["protocol"][..]),
        // `semantic` is the adapter-independent composition layer: it is the one
        // module allowed to name several subsystems at once, because deciding
        // how a Run starts and how a Run verb reaches its worker is exactly its
        // job.  Adapters (cli, main) still carry no product logic.
        (
            "semantic",
            &[
                "app_server",
                "artifact",
                "audit",
                "cli",
                "conformance",
                "controller",
                "darwin",
                "domain",
                "engagement",
                "event",
                "global_profile",
                "global_runtime",
                "interaction",
                "interaction_payload",
                "jcs",
                "ledger",
                "machine",
                "mutation_admission",
                "orchestration",
                "paths",
                "profile",
                "projection",
                "run",
                "runtime",
                "snapshot",
                "specialist",
                "specialist_policy",
                "turn",
                "worker",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "specialist",
            &["domain", "jcs", "machine", "run", "turn", "workspace"][..],
        ),
        ("task_request", &["machine"][..]),
        (
            "specialist_policy",
            &[
                "darwin",
                "domain",
                "jcs",
                "machine",
                "paths",
                "run",
                "semantic",
                "workspace",
            ][..],
        ),
        (
            "turn",
            &[
                "app_server",
                "audit",
                "darwin",
                "domain",
                "fault",
                "interaction_payload",
                "jcs",
                "ledger",
                "machine",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "workspace",
            &["darwin", "jcs", "machine", "paths", "writer"][..],
        ),
        // Writer status reuses the semantic Run status projection; its local
        // regression fixture also constructs a RunStateProjection directly.
        (
            "writer",
            &[
                "darwin",
                "domain",
                "machine",
                "projection",
                "run",
                "semantic",
                "workspace",
            ][..],
        ),
        // `worker` names `controller` because ADR-016 makes the hidden worker
        // the authoritative consumer of a Controller credential: it rereads
        // the descriptor it received over SCM_RIGHTS and revalidates it under
        // the Run mutation lock immediately before effects. That authority
        // check cannot be delegated to a caller without giving up the very
        // property the ADR requires.
        //
        // It names `profile` because a foreign-thread observation "is never a
        // Run event and uses the separate profile diagnostic schema"
        // (docs/specs/README.md), and the worker is the only place that knows both the
        // Run's coordinator and, from its own session bootstrap, which Runtime
        // Profile the Run is pinned to. `turn` stays free of it: the
        // coordinator is handed a writer rather than deriving one.
        // It names `writer` to refresh the current holder after control-socket
        // recovery, within the worker's writer-authority transaction boundary.
        (
            "worker",
            &[
                "app_server",
                "audit",
                "conformance",
                "controller",
                "darwin",
                "domain",
                "engagement",
                "event",
                "fault",
                "jcs",
                "ledger",
                "machine",
                "mutation_admission",
                "profile",
                "projection",
                "providers",
                "run",
                "turn",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "gateway",
            &["darwin", "gateway_socket", "machine", "paths", "protocol"][..],
        ),
        (
            "gateway_socket",
            &["darwin", "machine", "paths", "protocol"][..],
        ),
        (
            "gateway_projection",
            &[
                "domain",
                "machine",
                "profile",
                "protocol",
                "run",
                "runtime",
                "snapshot",
                "turn",
                "worker",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "gateway_event",
            &[
                "audit",
                "domain",
                "event",
                "gateway_projection",
                "machine",
                "protocol",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "gateway_service",
            &[
                "controller",
                "darwin",
                "domain",
                "gateway",
                "gateway_event",
                "gateway_observation",
                "gateway_projection",
                "global_profile",
                "ledger",
                "machine",
                "paths",
                "profile",
                "protocol",
                "run",
                "runtime",
                "semantic",
                "snapshot",
                "turn",
                "worker",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "snapshot",
            &[
                "audit",
                "controller",
                "darwin",
                "domain",
                "ledger",
                "machine",
                "projection",
                "run",
                "turn",
                "worker",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "gateway_observation",
            &[
                "artifact",
                "controller",
                "domain",
                "event",
                "gateway",
                "gateway_event",
                "gateway_projection",
                "interaction",
                "interaction_payload",
                "ledger",
                "machine",
                "protocol",
                "semantic",
                "snapshot",
                "turn",
                "workspace",
            ][..],
        ),
        (
            "artifact",
            &[
                "audit",
                "controller",
                "darwin",
                "event",
                "interaction_payload",
                "jcs",
                "ledger",
                "machine",
                "snapshot",
            ][..],
        ),
    ]);

    for path in rust_sources(&repository_root().join("src")) {
        let module = path.file_stem().unwrap().to_str().unwrap();
        if matches!(module, "lib" | "main") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        let observed = direct_crate_dependencies(&source);
        let expected = approved
            .get(module)
            .unwrap_or_else(|| panic!("missing architecture entry for {module}"))
            .iter()
            .map(|dependency| (*dependency).to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            observed,
            expected,
            "direct module dependencies changed for {}",
            path.display()
        );
    }
}

#[test]
fn unsafe_implementation_remains_inside_the_darwin_adapter() {
    for path in rust_sources(&repository_root().join("src")) {
        if path.file_name().is_some_and(|name| name == "darwin.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for forbidden in ["unsafe {", "unsafe fn ", "unsafe impl ", "unsafe trait "] {
            assert!(
                !source.contains(forbidden),
                "{} contains {forbidden:?}; unsafe code belongs in src/darwin.rs",
                path.display()
            );
        }
    }
}

#[test]
fn task_038_activates_one_global_runtime_selector() {
    let root = repository_root().join("src");
    let main = fs::read_to_string(root.join("main.rs")).unwrap();
    let semantic = fs::read_to_string(root.join("semantic.rs")).unwrap();
    assert!(main.contains("require_generation"));
    assert!(main.contains("execute_global"));
    assert!(semantic.contains("ResolvedGlobalProfile"));
    assert!(semantic.contains("global_profile_binding"));
}

#[test]
fn integration_tests_do_not_cross_the_external_system_boundary() {
    let forbidden = [
        "CARGO_BIN_EXE_dolgorae",
        "Command::new(\"git\")",
        "TcpListener",
        "TcpStream",
        "UdpSocket",
        "DATABASE_URL",
        "postgres://",
        "mysql://",
        "sqlite://",
        "http://",
        "https://",
    ];
    for path in rust_sources(&repository_root().join("tests")) {
        // These black-box executables belong to test-e2e and are never in INT_TESTS.
        let relative = path.strip_prefix(repository_root()).unwrap().to_path_buf();
        if [
            "tests/gateway_native.rs",
            "tests/gateway_semantic_native.rs",
            "tests/gateway_configuration_native.rs",
            "tests/gateway_interaction_native.rs",
            "tests/support/gateway_native.rs",
        ]
        .iter()
        .any(|native| relative == Path::new(native))
        {
            continue;
        }
        if path
            .file_name()
            .is_some_and(|name| name == "architecture_contract.rs")
        {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for marker in forbidden {
            assert!(
                !source.contains(marker),
                "{} crosses the test-int boundary with {marker:?}",
                path.display()
            );
        }
    }
}

fn collect_python_files(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !matches!(name, "target" | "__pycache__") {
                collect_python_files(&path, output);
            }
        } else if path.extension().is_some_and(|extension| extension == "py") {
            output.push(path);
        }
    }
}

#[test]
fn python_is_limited_to_dev_tools_validators_fixtures_and_black_box_e2e() {
    let root = repository_root();
    let dev_aquarium_root = root.join("tools/dev_aquarium");
    let validator_root = root.join("tools/validators");
    // ADR-014 places exactly one Python fixture outside the validators: the
    // shared fake app-server, whose independence from the Rust ingest path is
    // the point of it existing.
    let fake_app_server_root = root.join("tools/fake_app_server");
    let e2e_root = root.join("tests/e2e");
    let mut files = Vec::new();
    collect_python_files(&root, &mut files);
    for path in files {
        assert!(
            path.starts_with(&dev_aquarium_root)
                || path.starts_with(&validator_root)
                || path.starts_with(&fake_app_server_root)
                || path.starts_with(&e2e_root),
            "Python is limited to tools/dev_aquarium, tools/validators, tools/fake_app_server, and tests/e2e: {}",
            path.display()
        );
        if path.starts_with(&e2e_root) || path.starts_with(&fake_app_server_root) {
            let source = fs::read_to_string(&path).unwrap();
            assert!(!source.contains("import dolgorae"));
            assert!(!source.contains("from dolgorae"));
        }
    }
}

#[test]
fn the_shared_fake_app_server_shares_no_parser_with_the_product() {
    let root = repository_root().join("tools/fake_app_server");
    let mut files = Vec::new();
    collect_python_files(&root, &mut files);
    assert!(
        files.iter().any(|path| path.ends_with("jsonlite.py")),
        "the fake app-server must carry its own strict JSON reader"
    );
    for path in files {
        if path.ends_with("jsonlite.py") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for forbidden in [
            "import json\n",
            "import json ",
            "from json import",
            "json.loads",
            "json.dumps",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} reaches for the stdlib JSON module; ADR-014 requires parser diversity",
                path.display()
            );
        }
    }
}

fn native_test_functions(source: &str) -> BTreeSet<String> {
    let mut test_attribute = false;
    let mut tests = BTreeSet::new();
    for line in source.lines().map(str::trim) {
        if line.starts_with("#[tokio::test") {
            test_attribute = true;
        } else if test_attribute && line.starts_with("async fn ") {
            let name = line["async fn ".len()..]
                .split_once('(')
                .expect("native test function declaration")
                .0;
            assert!(
                tests.insert(name.to_owned()),
                "duplicate native test {name}"
            );
            test_attribute = false;
        }
    }
    tests
}

fn quoted_arguments(source: &str, start: usize) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = &source[start..];
    while values.len() < 2 {
        let quote = rest.find('"').expect("run_case string argument");
        rest = &rest[quote + 1..];
        let end = rest.find('"').expect("terminated run_case string argument");
        values.push(rest[..end].to_owned());
        rest = &rest[end + 1..];
    }
    values
}

#[test]
fn every_native_gateway_test_has_exactly_one_e2e_wrapper_reference() {
    let root = repository_root();
    let native_targets = [
        "gateway_native",
        "gateway_semantic_native",
        "gateway_configuration_native",
        "gateway_interaction_native",
    ];
    let mut declared = BTreeSet::new();
    for target in native_targets {
        let source = fs::read_to_string(root.join("tests").join(format!("{target}.rs"))).unwrap();
        for test in native_test_functions(&source) {
            assert!(declared.insert(format!("{target}::{test}")));
        }
    }

    let mut referenced = BTreeMap::<String, usize>::new();
    let e2e = root.join("tests/e2e");
    for entry in fs::read_dir(&e2e).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "py") {
            continue;
        }
        if path
            .file_name()
            .is_some_and(|name| name == "run_native_gateway_case.py")
        {
            continue;
        }
        let source = fs::read_to_string(path).unwrap();
        for (index, _) in source.match_indices("run_case(") {
            let arguments = quoted_arguments(&source, index + "run_case(".len());
            *referenced
                .entry(format!("{}::{}", arguments[0], arguments[1]))
                .or_default() += 1;
        }
    }
    assert_eq!(
        referenced.keys().cloned().collect::<BTreeSet<_>>(),
        declared,
        "native gateway source tests and E2E wrapper cases diverged"
    );
    assert!(
        referenced.values().all(|count| *count == 1),
        "each native gateway test must be referenced exactly once: {referenced:?}"
    );
    let runner = fs::read_to_string(e2e.join("run_native_gateway_case.py")).unwrap();
    assert!(runner.contains("\"--list\", \"--format\", \"terse\""));
    assert!(runner.contains("native gateway case is unavailable"));
}
