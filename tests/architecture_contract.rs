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
        ("darwin", &["providers"][..]),
        ("domain", &[][..]),
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
        (
            "profile",
            &["app_server", "darwin", "jcs", "machine", "workspace"][..],
        ),
        ("projection", &["audit", "domain", "jcs"][..]),
        ("protocol", &[][..]),
        ("providers", &[][..]),
        (
            "run",
            &["audit", "domain", "jcs", "machine", "workspace"][..],
        ),
        ("runtime", &["protocol"][..]),
        ("semantic", &["machine", "runtime", "workspace"][..]),
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
        (
            "worker",
            &[
                "conformance",
                "darwin",
                "event",
                "fault",
                "ledger",
                "machine",
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
fn python_is_limited_to_validators_and_black_box_e2e() {
    let root = repository_root();
    let validator_root = root.join("tools/validators");
    let e2e_root = root.join("tests/e2e");
    let mut files = Vec::new();
    collect_python_files(&root, &mut files);
    for path in files {
        assert!(
            path.starts_with(&validator_root) || path.starts_with(&e2e_root),
            "Python is limited to tools/validators and tests/e2e: {}",
            path.display()
        );
        if path.starts_with(&e2e_root) {
            let source = fs::read_to_string(&path).unwrap();
            assert!(!source.contains("import dolgorae"));
            assert!(!source.contains("from dolgorae"));
        }
    }
}
