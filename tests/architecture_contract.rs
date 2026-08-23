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
        ("audit", &["jcs"][..]),
        ("cli", &[][..]),
        (
            "conformance",
            &[
                "audit",
                "domain",
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
                "projection",
                "run",
                "workspace",
            ][..],
        ),
        ("darwin", &["providers"][..]),
        ("domain", &[][..]),
        (
            "engagement",
            &["domain", "jcs", "machine", "run", "specialist", "workspace"][..],
        ),
        ("event", &["audit", "domain", "jcs", "workspace"][..]),
        ("fault", &[][..]),
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
        ("machine", &["workspace"][..]),
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
                "jcs",
                "machine",
                "workspace",
            ][..],
        ),
        ("projection", &["audit", "domain", "jcs"][..]),
        ("protocol", &[][..]),
        ("providers", &[][..]),
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
                "semantic",
                "specialist",
                "workspace",
            ][..],
        ),
        // `run` names `projection` because the Run record and the Run's
        // durable state projection are two halves of the same durable state: an
        // observer that may not take the ledger still has to read the
        // projection the ledger commits beside the manifest.
        (
            "run",
            &[
                "audit",
                "domain",
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
                "cli",
                "conformance",
                "controller",
                "darwin",
                "domain",
                "event",
                "jcs",
                "ledger",
                "machine",
                "profile",
                "projection",
                "run",
                "runtime",
                "specialist",
                "turn",
                "worker",
                "workspace",
            ][..],
        ),
        (
            "specialist",
            &["domain", "jcs", "machine", "run", "turn", "workspace"][..],
        ),
        (
            "turn",
            &[
                "app_server",
                "audit",
                "fault",
                "jcs",
                "ledger",
                "machine",
                "workspace",
            ][..],
        ),
        ("workspace", &["darwin", "jcs", "machine"][..]),
        // `worker` names `controller` because ADR-016 makes the hidden worker
        // the authoritative consumer of a Controller credential: it rereads
        // the descriptor it received over SCM_RIGHTS and revalidates it under
        // the Run mutation lock immediately before effects. That authority
        // check cannot be delegated to a caller without giving up the very
        // property the ADR requires.
        //
        // It names `profile` because a foreign-thread observation "is never a
        // Run event and uses the separate profile diagnostic schema"
        // (specs.md), and the worker is the only place that knows both the
        // Run's coordinator and, from its own session bootstrap, which Runtime
        // Profile the Run is pinned to. `turn` stays free of it: the
        // coordinator is handed a writer rather than deriving one.
        (
            "worker",
            &[
                "app_server",
                "conformance",
                "controller",
                "darwin",
                "event",
                "fault",
                "ledger",
                "machine",
                "profile",
                "turn",
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
fn python_is_limited_to_validators_fixtures_and_black_box_e2e() {
    let root = repository_root();
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
            path.starts_with(&validator_root)
                || path.starts_with(&fake_app_server_root)
                || path.starts_with(&e2e_root),
            "Python is limited to tools/validators, tools/fake_app_server, and tests/e2e: {}",
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
