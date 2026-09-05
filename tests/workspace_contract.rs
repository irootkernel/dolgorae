#![cfg(target_os = "macos")]

use dolgorae::workspace::{
    PlatformFilesystem, SystemGitRunner, SystemWorkspacePlatform, WorkspaceMode, WorkspacePlatform,
    WorkspaceService, workspace_id,
};
use std::fs;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};

struct TestTree(PathBuf);

impl TestTree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("dolgorae-task002-{}", uuid::Uuid::now_v7()));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(&path).unwrap();
        Self(path)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        if self.0.starts_with(std::env::temp_dir())
            && self
                .0
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("dolgorae-task002-"))
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn make_dir(path: &Path) {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).recursive(true).create(path).unwrap();
}

#[test]
fn non_git_initialization_discovers_upward_and_is_idempotent() {
    let tree = TestTree::new();
    let workspace = tree.path("workspace");
    let nested = workspace.join("a/b");
    make_dir(&nested);
    make_dir(&tree.path("support"));
    let service = WorkspaceService::new(
        SystemWorkspacePlatform,
        SystemGitRunner,
        tree.path("support/Dolgorae"),
    );

    let created = service
        .initialize(Some(&workspace), WorkspaceMode::NonGit)
        .unwrap();
    assert!(created.created);
    assert_eq!(created.mode, WorkspaceMode::NonGit);
    assert_eq!(
        service.discover_from(&nested, None).unwrap().workspace_id,
        created.workspace_id
    );
    assert!(
        !service
            .initialize(Some(&workspace), WorkspaceMode::NonGit)
            .unwrap()
            .created
    );
    assert_eq!(
        fs::read(workspace.join(".dolgorae/.gitignore")).unwrap(),
        b"/exports/\n"
    );
    fs::write(
        workspace.join(".dolgorae/config.yaml"),
        "mode: non_git\nschema_version: 1\n",
    )
    .unwrap();
    assert!(
        !service
            .initialize(Some(&workspace), WorkspaceMode::NonGit)
            .unwrap()
            .created
    );
    assert_eq!(
        fs::read_to_string(workspace.join(".dolgorae/config.yaml")).unwrap(),
        "mode: non_git\nschema_version: 1\n"
    );
}

#[test]
fn nested_non_git_mode_change_and_missing_or_replaced_lock_are_refused() {
    let tree = TestTree::new();
    let workspace = tree.path("workspace");
    let nested = workspace.join("nested");
    make_dir(&nested);
    make_dir(&tree.path("support"));
    let support = tree.path("support/Dolgorae");
    let service = WorkspaceService::new(SystemWorkspacePlatform, SystemGitRunner, support.clone());
    let view = service
        .initialize(Some(&workspace), WorkspaceMode::NonGit)
        .unwrap();
    assert!(
        service
            .initialize(Some(&nested), WorkspaceMode::NonGit)
            .is_err()
    );

    fs::write(
        workspace.join(".dolgorae/config.yaml"),
        "schema_version: 1\nmode: git\n",
    )
    .unwrap();
    assert!(
        service
            .initialize(Some(&workspace), WorkspaceMode::NonGit)
            .is_err()
    );
    fs::write(
        workspace.join(".dolgorae/config.yaml"),
        "schema_version: 1\nmode: non_git\n",
    )
    .unwrap();

    let lock_root = support
        .join("workspaces")
        .join(&view.workspace_id)
        .join("runtime/locks");
    fs::set_permissions(&lock_root, fs::Permissions::from_mode(0o777)).unwrap();
    let error = service.discover_from(&workspace, None).unwrap_err();
    assert_eq!(error.code, "RUNTIME_PATH_INVALID");
    fs::set_permissions(&lock_root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(lock_root.join("writer.lock")).unwrap();
    fs::remove_file(lock_root.join("handoff.lock")).unwrap();
    fs::remove_dir(lock_root.join("startup")).unwrap();
    fs::remove_dir(&lock_root).unwrap();
    let error = service.discover_from(&workspace, None).unwrap_err();
    assert_eq!(error.code, "RUNTIME_PATH_COLLISION");
    make_dir(&lock_root);
    fs::set_permissions(&lock_root, fs::Permissions::from_mode(0o700)).unwrap();
    let error = service.discover_from(&workspace, None).unwrap_err();
    assert_eq!(error.code, "RUNTIME_PATH_COLLISION");
}

#[test]
fn partial_layout_is_an_initialization_conflict() {
    let tree = TestTree::new();
    let workspace = tree.path("workspace");
    make_dir(&workspace.join(".dolgorae"));
    fs::write(
        workspace.join(".dolgorae/config.yaml"),
        "schema_version: 1\nmode: non_git\n",
    )
    .unwrap();
    let service = WorkspaceService::new(
        SystemWorkspacePlatform,
        SystemGitRunner,
        tree.path("support/Dolgorae"),
    );
    let error = service
        .initialize(Some(&workspace), WorkspaceMode::NonGit)
        .unwrap_err();
    assert_eq!(error.code, "WORKSPACE_INITIALIZATION_CONFLICT");
}

#[test]
fn dolgorae_home_authority_cannot_overlap_the_workspace() {
    for use_symlink in [false, true] {
        let tree = TestTree::new();
        let workspace = tree.path("workspace");
        make_dir(&workspace);
        let support = if use_symlink {
            let target = workspace.join("mutable");
            make_dir(&target);
            let alias = tree.path("support-alias");
            symlink(&target, &alias).unwrap();
            alias.join("Dolgorae")
        } else {
            workspace.join("mutable/Dolgorae")
        };
        let service = WorkspaceService::new(SystemWorkspacePlatform, SystemGitRunner, support);
        let error = service
            .initialize(Some(&workspace), WorkspaceMode::NonGit)
            .unwrap_err();
        assert_eq!(error.code, "RUNTIME_PATH_INVALID");
        assert!(!workspace.join(".dolgorae").exists());
    }
}

#[test]
fn init_and_discovery_emit_context_specific_workspace_errors() {
    let tree = TestTree::new();
    let workspace = tree.path("workspace");
    make_dir(&workspace);
    make_dir(&tree.path("support"));
    let service = WorkspaceService::new(
        SystemWorkspacePlatform,
        SystemGitRunner,
        tree.path("support/Dolgorae"),
    );
    let missing = tree.path("missing");
    assert_eq!(
        service.discover(Some(&missing)).unwrap_err().code,
        "WORKSPACE_NOT_INITIALIZED"
    );
    service
        .initialize(Some(&workspace), WorkspaceMode::NonGit)
        .unwrap();
    fs::write(workspace.join(".dolgorae/config.yaml"), "not: [valid").unwrap();
    assert_eq!(
        service
            .initialize(Some(&workspace), WorkspaceMode::NonGit)
            .unwrap_err()
            .code,
        "WORKSPACE_INITIALIZATION_CONFLICT"
    );
    assert_eq!(
        service.discover(Some(&workspace)).unwrap_err().code,
        "CONFIG_INVALID"
    );
}

#[test]
fn workspace_discovery_ignores_legacy_local_profile_files() {
    let tree = TestTree::new();
    let workspace = tree.path("workspace");
    make_dir(&workspace);
    make_dir(&tree.path("support"));
    let support = tree.path("support/Dolgorae");
    let service = WorkspaceService::new(SystemWorkspacePlatform, SystemGitRunner, support.clone());
    let view = service
        .initialize(Some(&workspace), WorkspaceMode::NonGit)
        .unwrap();
    let local = support
        .join("workspaces")
        .join(&view.workspace_id)
        .join("local.yaml");
    fs::write(
        &local,
        "schema_version: 1\nprofiles:\n  bad:\n    argv: [/bin/sh, app-server]\n    codex_home: /tmp/codex-home\n    environment: {PATH: /bin, LANG: C, LC_ALL: C}\n    native_subagents: enabled\n",
    )
    .unwrap();
    assert!(service.discover_for_run_start(Some(&workspace)).is_ok());
}

#[derive(Clone, Copy)]
struct FilesystemOverride {
    local: bool,
    apfs: bool,
}

impl WorkspacePlatform for FilesystemOverride {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
        SystemWorkspacePlatform.canonicalize(path)
    }

    fn filesystem_info(&self, _path: &Path) -> Result<PlatformFilesystem, std::io::Error> {
        Ok(PlatformFilesystem {
            local: self.local,
            filesystem_type: if self.apfs { "apfs" } else { "other" }.to_owned(),
        })
    }

    fn current_uid(&self) -> u32 {
        SystemWorkspacePlatform.current_uid()
    }

    fn rename_exclusive(&self, source: &Path, destination: &Path) -> Result<(), std::io::Error> {
        SystemWorkspacePlatform.rename_exclusive(source, destination)
    }
}

#[test]
fn nonlocal_and_non_apfs_filesystems_are_refused_without_override() {
    for platform in [
        FilesystemOverride {
            local: false,
            apfs: true,
        },
        FilesystemOverride {
            local: true,
            apfs: false,
        },
    ] {
        let tree = TestTree::new();
        let workspace = tree.path("workspace");
        make_dir(&workspace);
        let service =
            WorkspaceService::new(platform, SystemGitRunner, tree.path("support/Dolgorae"));
        let error = service
            .initialize(Some(&workspace), WorkspaceMode::NonGit)
            .unwrap_err();
        assert_eq!(error.code, "RUNTIME_PATH_INVALID");
    }
}

#[test]
fn workspace_id_keeps_case_distinct() {
    assert_ne!(
        workspace_id(Path::new("/tmp/a")),
        workspace_id(Path::new("/tmp/A"))
    );
}

#[test]
fn libc_realpath_normalizes_symlinks_and_obeys_volume_case_semantics() {
    let tree = TestTree::new();
    let target = tree.path("RealWorkspace");
    make_dir(&target);
    let alias = tree.path("alias");
    symlink(&target, &alias).unwrap();
    make_dir(&tree.path("support"));
    let service = WorkspaceService::new(
        SystemWorkspacePlatform,
        SystemGitRunner,
        tree.path("support/Dolgorae"),
    );
    let view = service
        .initialize(Some(&alias), WorkspaceMode::NonGit)
        .unwrap();
    assert_eq!(
        view.canonical_path.to_path_buf().unwrap(),
        SystemWorkspacePlatform.canonicalize(&target).unwrap()
    );

    let differently_cased = tree.path("realworkspace");
    if differently_cased.exists() {
        assert_eq!(
            SystemWorkspacePlatform.canonicalize(&target).unwrap(),
            SystemWorkspacePlatform
                .canonicalize(&differently_cased)
                .unwrap()
        );
    } else {
        make_dir(&differently_cased);
        let first = SystemWorkspacePlatform.canonicalize(&target).unwrap();
        let second = SystemWorkspacePlatform
            .canonicalize(&differently_cased)
            .unwrap();
        assert_ne!(first, second);
        assert_ne!(workspace_id(&first), workspace_id(&second));
    }
}

#[test]
fn data_volume_alias_is_substituted_only_for_equal_identity() {
    let data_users = PathBuf::from("/System/Volumes/Data/Users");
    let users = PathBuf::from("/Users");
    let shared_identity = dolgorae::workspace::FileIdentity {
        device: 42,
        inode: 7,
    };
    let normalized = dolgorae::workspace::normalize_data_volume_alias(data_users.clone(), |path| {
        (path == data_users || path == users).then_some(shared_identity)
    })
    .unwrap();
    assert_eq!(normalized, Path::new("/Users"));

    let unchanged = dolgorae::workspace::normalize_data_volume_alias(data_users.clone(), |path| {
        (path == data_users).then_some(shared_identity)
    })
    .unwrap();
    assert_eq!(unchanged, data_users);
}

#[derive(Clone)]
struct GitWorktree(PathBuf);

impl dolgorae::workspace::GitRunner for GitWorktree {
    fn output(
        &self,
        arguments: &[std::ffi::OsString],
    ) -> Result<std::process::Output, std::io::Error> {
        let stdout = if arguments == [std::ffi::OsString::from("--version")] {
            b"git version 2.39.0\n".to_vec()
        } else if arguments
            .windows(2)
            .any(|values| values[0] == "rev-parse" && values[1] == "--show-toplevel")
        {
            format!("{}\n", self.0.display()).into_bytes()
        } else {
            panic!("unexpected fake Git arguments: {arguments:?}")
        };
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout,
            stderr: Vec::new(),
        })
    }
}

#[test]
fn explicit_non_git_initialization_is_refused_inside_git() {
    let tree = TestTree::new();
    let repository = tree.path("repository");
    make_dir(&repository);
    make_dir(&tree.path("support"));
    let service = WorkspaceService::new(
        SystemWorkspacePlatform,
        GitWorktree(repository.clone()),
        tree.path("support/Dolgorae"),
    );
    let error = service
        .initialize(Some(&repository), WorkspaceMode::NonGit)
        .unwrap_err();
    assert_eq!(error.code, "WORKSPACE_INITIALIZATION_CONFLICT");
}

#[derive(Clone, Copy)]
struct MissingGit;

impl dolgorae::workspace::GitRunner for MissingGit {
    fn output(
        &self,
        _arguments: &[std::ffi::OsString],
    ) -> Result<std::process::Output, std::io::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "git is absent",
        ))
    }
}

#[test]
fn git_initialization_refuses_an_absent_git_binary() {
    let tree = TestTree::new();
    let repository = tree.path("repository");
    make_dir(&repository);
    make_dir(&tree.path("support"));
    let service = WorkspaceService::new(
        SystemWorkspacePlatform,
        MissingGit,
        tree.path("support/Dolgorae"),
    );
    let error = service
        .initialize(Some(&repository), WorkspaceMode::Git)
        .unwrap_err();
    assert_eq!(error.code, "WORKSPACE_INITIALIZATION_CONFLICT");
}

#[derive(Clone, Copy)]
struct OldGit;

impl dolgorae::workspace::GitRunner for OldGit {
    fn output(
        &self,
        _arguments: &[std::ffi::OsString],
    ) -> Result<std::process::Output, std::io::Error> {
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: b"git version 2.38.0\n".to_vec(),
            stderr: Vec::new(),
        })
    }
}

#[test]
fn git_initialization_refuses_versions_older_than_239() {
    let tree = TestTree::new();
    let repository = tree.path("repository");
    make_dir(&repository);
    make_dir(&tree.path("support"));
    let service = WorkspaceService::new(
        SystemWorkspacePlatform,
        OldGit,
        tree.path("support/Dolgorae"),
    );
    let error = service
        .initialize(Some(&repository), WorkspaceMode::Git)
        .unwrap_err();
    assert_eq!(error.code, "WORKSPACE_INITIALIZATION_CONFLICT");
}

#[test]
fn non_utf8_path_encoding_and_workspace_digest_are_lossless() {
    let path = PathBuf::from(std::ffi::OsString::from_vec(
        b"/tmp/workspace-\xff".to_vec(),
    ));
    let encoded = dolgorae::workspace::LosslessPath::from_path(&path);
    assert!(matches!(
        encoded,
        dolgorae::workspace::LosslessPath::Bytes { .. }
    ));
    assert_eq!(encoded.to_path_buf().unwrap(), path);
    assert_ne!(
        workspace_id(&path),
        workspace_id(Path::new("/tmp/workspace"))
    );
}
