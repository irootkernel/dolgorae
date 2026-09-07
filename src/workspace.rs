#[cfg(target_os = "macos")]
use crate::darwin::{DarwinSystem, FilesystemInfo};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const CONFIG_YAML: &[u8] = b"schema_version: 1\nmode: git\n";
const NON_GIT_CONFIG_YAML: &[u8] = b"schema_version: 1\nmode: non_git\n";
const PROJECT_IGNORE: &[u8] = b"/exports/\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    Git,
    NonGit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LosslessPath {
    Utf8(String),
    Bytes {
        #[serde(rename = "$dolgorae_path_bytes")]
        bytes: String,
    },
}

impl LosslessPath {
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        match path.as_os_str().to_str() {
            Some(value) => Self::Utf8(value.to_owned()),
            None => Self::Bytes {
                bytes: base64::engine::general_purpose::STANDARD
                    .encode(path.as_os_str().as_bytes()),
            },
        }
    }

    pub fn to_path_buf(&self) -> Result<PathBuf, MachineError> {
        match self {
            Self::Utf8(value) => Ok(PathBuf::from(value)),
            Self::Bytes { bytes } => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(bytes)
                    .map_err(|error| {
                        MachineError::config_invalid("workspace.json", error.to_string())
                    })?;
                if base64::engine::general_purpose::STANDARD.encode(&decoded) != *bytes {
                    return Err(MachineError::config_invalid(
                        "workspace.json",
                        "path bytes are not canonical padded base64",
                    ));
                }
                Ok(PathBuf::from(OsString::from_vec(decoded)))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitBaseline {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub tracked_changes: Vec<LosslessPath>,
    pub untracked_paths: Vec<LosslessPath>,
}

impl GitBaseline {
    pub const fn empty() -> Self {
        Self {
            head: None,
            branch: None,
            tracked_changes: Vec::new(),
            untracked_paths: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecord {
    pub schema_version: u32,
    pub workspace_id: String,
    pub canonical_path: LosslessPath,
    pub mode: WorkspaceMode,
    pub initial_git_baseline: GitBaseline,
    pub state_root_identity: FileIdentity,
    pub lock_root_identity: FileIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceView {
    pub workspace_id: String,
    pub canonical_path: LosslessPath,
    pub mode: WorkspaceMode,
    pub created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortablePolicy {
    pub schema_version: u32,
    pub mode: WorkspaceMode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalProfileRegistry {
    pub schema_version: u32,
    pub profiles: BTreeMap<String, RuntimeProfile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProfile {
    pub argv: Vec<String>,
    pub codex_home: String,
    pub environment: BTreeMap<String, String>,
    pub native_subagents: NativeSubagents,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSubagents {
    Enabled,
}

pub trait WorkspacePlatform: Send + Sync {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, std::io::Error>;
    fn filesystem_info(&self, path: &Path) -> Result<PlatformFilesystem, std::io::Error>;
    fn current_uid(&self) -> u32;
    fn rename_exclusive(&self, source: &Path, destination: &Path) -> Result<(), std::io::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFilesystem {
    pub local: bool,
    pub filesystem_type: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWorkspacePlatform;

#[cfg(target_os = "macos")]
impl WorkspacePlatform for SystemWorkspacePlatform {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
        DarwinSystem.realpath(path)
    }

    fn filesystem_info(&self, path: &Path) -> Result<PlatformFilesystem, std::io::Error> {
        let FilesystemInfo {
            local,
            filesystem_type,
        } = DarwinSystem.filesystem_info(path)?;
        Ok(PlatformFilesystem {
            local,
            filesystem_type,
        })
    }

    fn current_uid(&self) -> u32 {
        DarwinSystem.current_uid()
    }

    fn rename_exclusive(&self, source: &Path, destination: &Path) -> Result<(), std::io::Error> {
        DarwinSystem.rename_exclusive(source, destination)
    }
}

#[cfg(not(target_os = "macos"))]
impl WorkspacePlatform for SystemWorkspacePlatform {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, std::io::Error> {
        fs::canonicalize(path)
    }

    fn filesystem_info(&self, _path: &Path) -> Result<PlatformFilesystem, std::io::Error> {
        Ok(PlatformFilesystem {
            local: false,
            filesystem_type: "unsupported".to_owned(),
        })
    }

    fn current_uid(&self) -> u32 {
        0
    }

    fn rename_exclusive(&self, source: &Path, destination: &Path) -> Result<(), std::io::Error> {
        fs::hard_link(source, destination)?;
        fs::remove_file(source)
    }
}

pub trait GitRunner: Send + Sync {
    fn output(&self, arguments: &[OsString]) -> Result<Output, std::io::Error>;
}

/// Creates one detached Git worktree for a caller-owned isolated workspace.
/// Process invocation remains inside the workspace boundary; the caller owns
/// the destination policy and lifecycle.
pub(crate) fn add_detached_git_worktree(canonical: &Path, root: &Path) -> std::io::Result<()> {
    let status = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(canonical)
        .args(["worktree", "add", "--detach"])
        .arg(root)
        .arg("HEAD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "detached Git worktree creation failed",
        ))
    }
}

#[must_use]
pub(crate) fn isolated_specialist_root(
    state_root: &Path,
    engagement_id: uuid::Uuid,
    run_id: uuid::Uuid,
) -> PathBuf {
    state_root
        .join("orchestration")
        .join("isolated")
        .join(engagement_id.to_string())
        .join(run_id.to_string())
}

pub(crate) fn is_registered_git_worktree(canonical: &Path, root: &Path) -> bool {
    fn identity(root: &Path, argument: &str) -> Option<PathBuf> {
        let mut child = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", argument])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut output = Vec::new();
        if child
            .stdout
            .take()?
            .take(16 * 1024 + 1)
            .read_to_end(&mut output)
            .is_err()
            || output.len() > 16 * 1024
        {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        if !child.wait().ok()?.success() {
            return None;
        }
        let value = std::str::from_utf8(&output).ok()?.trim();
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        path.canonicalize().ok()
    }

    let Some(canonical_root) = root.canonicalize().ok() else {
        return false;
    };
    identity(root, "--show-toplevel").is_some_and(|path| path == canonical_root)
        && identity(root, "--git-common-dir") == identity(canonical, "--git-common-dir")
}

pub(crate) fn open_relative_nofollow(
    directory: &File,
    name: &OsStr,
    require_directory: bool,
) -> std::io::Result<File> {
    #[cfg(target_os = "macos")]
    {
        DarwinSystem.openat_nofollow(directory, name, require_directory)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (directory, name, require_directory);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "descriptor-relative worktree capture is supported on macOS only",
        ))
    }
}

pub(crate) fn remove_git_worktree(canonical: &Path, root: &Path) -> std::io::Result<()> {
    let status = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(canonical)
        .args(["worktree", "remove", "--force"])
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("Git worktree removal failed"))
    }
}

pub(crate) fn capture_git_worktree_patch(
    root: &Path,
    maximum_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let intent = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(root)
        .args(["add", "--intent-to-add", "--all", "--"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !intent.success() {
        return Err(std::io::Error::other("Git worktree intent-to-add failed"));
    }
    let mut child = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--binary", "--no-ext-diff", "HEAD", "--"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut patch = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("piped Git stdout")
        .take(u64::try_from(maximum_bytes).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut patch);
    if let Err(error) = read {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    if patch.len() > maximum_bytes {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other(
            "Git worktree patch exceeds its bound",
        ));
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(std::io::Error::other("Git worktree patch is unavailable"));
    }
    Ok(patch)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemGitRunner;

impl GitRunner for SystemGitRunner {
    fn output(&self, arguments: &[OsString]) -> Result<Output, std::io::Error> {
        let limit = if arguments.iter().any(|value| value == "status") {
            64 * 1024 * 1024
        } else {
            1024 * 1024
        };
        let mut child = Command::new("git")
            .args(arguments)
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdout = Vec::new();
        child
            .stdout
            .take()
            .expect("piped stdout")
            .take(u64::try_from(limit + 1).expect("small bound"))
            .read_to_end(&mut stdout)?;
        if stdout.len() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Git output exceeds the bounded capture limit",
            ));
        }
        let status = child.wait()?;
        Ok(Output {
            status,
            stdout,
            stderr: Vec::new(),
        })
    }
}

pub struct WorkspaceService<P = SystemWorkspacePlatform, G = SystemGitRunner> {
    platform: P,
    git: G,
    dolgorae_home_root: PathBuf,
}

impl WorkspaceService<SystemWorkspacePlatform, SystemGitRunner> {
    pub fn system() -> Result<Self, MachineError> {
        let platform = SystemWorkspacePlatform;
        let dolgorae_home = DolgoraeHome::system()?;
        Ok(Self::new(
            platform,
            SystemGitRunner,
            dolgorae_home.root().to_path_buf(),
        ))
    }
}

impl<P: WorkspacePlatform, G: GitRunner> WorkspaceService<P, G> {
    #[must_use]
    pub const fn new(platform: P, git: G, dolgorae_home_root: PathBuf) -> Self {
        Self {
            platform,
            git,
            dolgorae_home_root,
        }
    }

    pub fn initialize(
        &self,
        supplied: Option<&Path>,
        mode: WorkspaceMode,
    ) -> Result<WorkspaceView, MachineError> {
        self.initialize_with_preparation(supplied, mode, || Ok(()))
    }

    /// Prepare shared state only after workspace admission has succeeded.
    pub(crate) fn initialize_with_preparation(
        &self,
        supplied: Option<&Path>,
        mode: WorkspaceMode,
        prepare: impl FnOnce() -> Result<(), MachineError>,
    ) -> Result<WorkspaceView, MachineError> {
        let supplied = supplied
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| {
                MachineError::initialization_conflict(".", "current directory unavailable")
            })?;
        if !supplied.is_dir() {
            return Err(MachineError::initialization_conflict(
                &supplied,
                "initialization path is not an existing directory",
            ));
        }

        let canonical = match mode {
            WorkspaceMode::Git => self.discover_git_root(&supplied)?,
            WorkspaceMode::NonGit => {
                if self.try_discover_git_root(&supplied)?.is_some() {
                    return Err(MachineError::initialization_conflict(
                        &supplied,
                        "--non-git is forbidden inside a Git worktree",
                    ));
                }
                self.canonical_workspace(&supplied)?
            }
        };
        self.require_local_apfs(&canonical)?;
        self.reject_nested_workspace(&canonical)?;

        let workspace_id = workspace_id(&canonical);
        let policy_root = canonical.join(".dolgorae");
        let state_root = self
            .dolgorae_home_root
            .join("workspaces")
            .join(&workspace_id);
        self.require_state_separation(&canonical, &state_root)?;
        let policy_exists = fs::symlink_metadata(&policy_root).is_ok();
        let state_exists = fs::symlink_metadata(&state_root).is_ok();
        if state_exists {
            if !policy_exists {
                return Err(MachineError::initialization_conflict(
                    &canonical,
                    "partial workspace layout",
                ));
            }
            let record = self
                .validate_existing(
                    &canonical,
                    mode,
                    &workspace_id,
                    &policy_root,
                    &state_root,
                    true,
                )
                .map_err(|error| reinitialization_error(&canonical, error))?;
            prepare()?;
            return Ok(view_from_record(record, false));
        }

        if policy_exists {
            self.validate_portable_policy(&canonical, mode, &policy_root)
                .map_err(|error| reinitialization_error(&canonical, error))?;
            let baseline = match mode {
                WorkspaceMode::Git => self.capture_git_baseline(&canonical)?,
                WorkspaceMode::NonGit => GitBaseline::empty(),
            };
            prepare()?;
            self.create_machine_layout(&canonical, mode, &workspace_id, &state_root, baseline)?;
            let record = self.validate_existing(
                &canonical,
                mode,
                &workspace_id,
                &policy_root,
                &state_root,
                false,
            )?;
            return Ok(view_from_record(record, true));
        }

        let baseline = match mode {
            WorkspaceMode::Git => self.capture_git_baseline(&canonical)?,
            WorkspaceMode::NonGit => GitBaseline::empty(),
        };
        prepare()?;
        self.create_new_layout(
            &canonical,
            mode,
            &workspace_id,
            &policy_root,
            &state_root,
            baseline,
        )?;
        self.require_state_separation(&canonical, &state_root)?;
        let record = self.validate_existing(
            &canonical,
            mode,
            &workspace_id,
            &policy_root,
            &state_root,
            true,
        )?;
        Ok(view_from_record(record, true))
    }

    pub fn discover(&self, explicit: Option<&Path>) -> Result<WorkspaceView, MachineError> {
        let current = std::env::current_dir()
            .map_err(|error| MachineError::workspace_not_initialized(error.to_string()))?;
        self.discover_from_internal(&current, explicit, false)
    }

    pub fn discover_for_run_start(
        &self,
        explicit: Option<&Path>,
    ) -> Result<WorkspaceView, MachineError> {
        let current = std::env::current_dir()
            .map_err(|error| MachineError::workspace_not_initialized(error.to_string()))?;
        self.discover_from_internal(&current, explicit, true)
    }

    /// Capture the working tree exactly when a Run is allocated.
    ///
    /// Workspace initialization records its own historical baseline, but a
    /// Run must pin the current tree so later observations can distinguish
    /// pre-existing changes from work performed during that Run.
    pub fn capture_run_baseline(
        &self,
        workspace: &WorkspaceView,
    ) -> Result<GitBaseline, MachineError> {
        match workspace.mode {
            WorkspaceMode::Git => {
                let root = workspace.canonical_path.to_path_buf()?;
                self.capture_git_baseline(&root)
            }
            WorkspaceMode::NonGit => Ok(GitBaseline::empty()),
        }
    }

    pub fn discover_from(
        &self,
        start: &Path,
        explicit: Option<&Path>,
    ) -> Result<WorkspaceView, MachineError> {
        self.discover_from_internal(start, explicit, false)
    }

    fn discover_from_internal(
        &self,
        start: &Path,
        explicit: Option<&Path>,
        validate_profiles: bool,
    ) -> Result<WorkspaceView, MachineError> {
        let selected = match explicit {
            Some(path) => {
                let canonical = self
                    .canonical_workspace(path)
                    .map_err(|_| MachineError::workspace_not_initialized(path))?;
                if !canonical.join(".dolgorae").is_dir() {
                    return Err(MachineError::workspace_not_initialized(&canonical));
                }
                canonical
            }
            None => {
                let canonical = self
                    .canonical_workspace(start)
                    .map_err(|_| MachineError::workspace_not_initialized(start))?;
                canonical
                    .ancestors()
                    .find(|candidate| candidate.join(".dolgorae").is_dir())
                    .map(Path::to_path_buf)
                    .ok_or_else(|| MachineError::workspace_not_initialized(&canonical))?
            }
        };
        let policy = parse_portable_policy(&selected.join(".dolgorae/config.yaml"))?;
        if policy.schema_version != 1 {
            return Err(MachineError::config_invalid(
                selected.join(".dolgorae/config.yaml"),
                "unsupported schema_version",
            ));
        }
        let canonical = match policy.mode {
            WorkspaceMode::Git => {
                let git_root = self
                    .discover_git_root(&selected)
                    .map_err(|_| MachineError::workspace_not_initialized(&selected))?;
                if git_root != selected {
                    return Err(MachineError::workspace_not_initialized(&selected));
                }
                git_root
            }
            WorkspaceMode::NonGit => self
                .canonical_workspace(&selected)
                .map_err(|_| MachineError::workspace_not_initialized(&selected))?,
        };
        self.require_local_apfs(&canonical)?;
        let id = workspace_id(&canonical);
        let state_root = self.dolgorae_home_root.join("workspaces").join(&id);
        self.require_state_separation(&canonical, &state_root)?;
        let record = self
            .validate_existing(
                &canonical,
                policy.mode,
                &id,
                &canonical.join(".dolgorae"),
                &state_root,
                validate_profiles,
            )
            .map_err(|error| {
                if error.code == "WORKSPACE_INITIALIZATION_CONFLICT" {
                    MachineError::workspace_not_initialized(&canonical)
                } else {
                    error
                }
            })?;
        Ok(view_from_record(record, false))
    }

    fn canonical_workspace(&self, path: &Path) -> Result<PathBuf, MachineError> {
        let canonical = self
            .platform
            .canonicalize(path)
            .map_err(|error| MachineError::initialization_conflict(path, error.to_string()))?;
        normalize_data_volume_alias(canonical, |candidate| file_identity(candidate).ok())
    }

    fn try_discover_git_root(&self, supplied: &Path) -> Result<Option<PathBuf>, MachineError> {
        self.require_git_version()?;
        let output = self
            .git
            .output(&git_arguments(supplied, &["rev-parse", "--show-toplevel"]))
            .map_err(|error| MachineError::initialization_conflict(supplied, error.to_string()))?;
        if !output.status.success() {
            return Ok(None);
        }
        let decoded = decode_git_toplevel(&output.stdout)
            .map_err(|reason| MachineError::initialization_conflict(supplied, reason))?;
        self.canonical_workspace(Path::new(&decoded)).map(Some)
    }

    fn discover_git_root(&self, supplied: &Path) -> Result<PathBuf, MachineError> {
        self.try_discover_git_root(supplied)?.ok_or_else(|| {
            MachineError::initialization_conflict(supplied, "Git workspace discovery failed")
        })
    }

    fn require_git_version(&self) -> Result<(), MachineError> {
        let output = self
            .git
            .output(&[OsString::from("--version")])
            .map_err(|error| MachineError::initialization_conflict("git", error.to_string()))?;
        if !output.status.success() {
            return Err(MachineError::initialization_conflict(
                "git",
                "git --version failed",
            ));
        }
        let text = std::str::from_utf8(&output.stdout)
            .map_err(|error| MachineError::initialization_conflict("git", error.to_string()))?;
        let version = text
            .strip_prefix("git version ")
            .and_then(|value| value.split_whitespace().next())
            .ok_or_else(|| {
                MachineError::initialization_conflict("git", "invalid git version output")
            })?;
        let mut numbers = version.split('.').map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
        });
        let major = numbers.next().and_then(Result::ok).unwrap_or(0);
        let minor = numbers.next().and_then(Result::ok).unwrap_or(0);
        if (major, minor) < (2, 39) {
            return Err(MachineError::initialization_conflict(
                "git",
                "Git 2.39 or later is required",
            ));
        }
        Ok(())
    }

    fn capture_git_baseline(&self, root: &Path) -> Result<GitBaseline, MachineError> {
        let head = self.optional_git_line(root, &["rev-parse", "--verify", "HEAD"])?;
        let branch = self.optional_git_line(root, &["symbolic-ref", "--short", "-q", "HEAD"])?;
        let output = self
            .git
            .output(&git_arguments(
                root,
                &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
            ))
            .map_err(|error| MachineError::initialization_conflict(root, error.to_string()))?;
        if !output.status.success() {
            return Err(MachineError::initialization_conflict(
                root,
                "git status baseline failed",
            ));
        }
        let (tracked_changes, untracked_paths) = parse_porcelain_v2_z(&output.stdout)?;
        Ok(GitBaseline {
            head,
            branch,
            tracked_changes,
            untracked_paths,
        })
    }

    fn optional_git_line(
        &self,
        root: &Path,
        command: &[&str],
    ) -> Result<Option<String>, MachineError> {
        let output = self
            .git
            .output(&git_arguments(root, command))
            .map_err(|error| MachineError::initialization_conflict(root, error.to_string()))?;
        if !output.status.success() {
            return Ok(None);
        }
        let line = output.stdout.strip_suffix(b"\n").ok_or_else(|| {
            MachineError::initialization_conflict(root, "Git baseline output lacks final LF")
        })?;
        if line.contains(&b'\n') || line.contains(&0) {
            return Err(MachineError::initialization_conflict(
                root,
                "invalid Git baseline output",
            ));
        }
        String::from_utf8(line.to_vec())
            .map(Some)
            .map_err(|error| MachineError::initialization_conflict(root, error.to_string()))
    }

    fn reject_nested_workspace(&self, canonical: &Path) -> Result<(), MachineError> {
        if let Some(parent) = canonical.parent()
            && let Some(owner) = parent.ancestors().find(|candidate| {
                let marker = candidate.join(".dolgorae");
                marker.is_dir() && marker != self.dolgorae_home_root
            })
        {
            return Err(MachineError::initialization_conflict(
                canonical,
                format!("nested below initialized workspace {}", owner.display()),
            ));
        }
        Ok(())
    }

    fn require_local_apfs(&self, path: &Path) -> Result<(), MachineError> {
        let info = self
            .platform
            .filesystem_info(path)
            .map_err(|error| MachineError::runtime_path_invalid(path, error.to_string()))?;
        if !info.local || info.filesystem_type != "apfs" {
            return Err(MachineError::runtime_path_invalid(
                path,
                format!(
                    "local APFS required (local={}, type={})",
                    info.local, info.filesystem_type
                ),
            ));
        }
        Ok(())
    }

    fn require_state_separation(
        &self,
        canonical_workspace: &Path,
        state_root: &Path,
    ) -> Result<(), MachineError> {
        let resolved_state_root = self.resolve_existing_prefix(state_root)?;
        if resolved_state_root.starts_with(canonical_workspace)
            || canonical_workspace.starts_with(&resolved_state_root)
        {
            return Err(MachineError::runtime_path_invalid(
                state_root,
                "Dolgorae home authority overlaps the canonical workspace",
            ));
        }
        Ok(())
    }

    fn resolve_existing_prefix(&self, path: &Path) -> Result<PathBuf, MachineError> {
        let mut existing = path;
        let mut suffix = Vec::new();
        while fs::symlink_metadata(existing).is_err() {
            let name = existing.file_name().ok_or_else(|| {
                MachineError::runtime_path_invalid(path, "path has no existing ancestor")
            })?;
            suffix.push(name.to_owned());
            existing = existing.parent().ok_or_else(|| {
                MachineError::runtime_path_invalid(path, "path has no existing ancestor")
            })?;
        }
        let mut resolved = self
            .platform
            .canonicalize(existing)
            .map_err(|error| MachineError::runtime_path_invalid(existing, error.to_string()))?;
        for component in suffix.into_iter().rev() {
            resolved.push(component);
        }
        Ok(resolved)
    }

    fn create_new_layout(
        &self,
        canonical: &Path,
        mode: WorkspaceMode,
        workspace_id: &str,
        policy_root: &Path,
        state_root: &Path,
        baseline: GitBaseline,
    ) -> Result<(), MachineError> {
        create_directory(policy_root, 0o755)
            .map_err(|error| MachineError::initialization_conflict(canonical, error.to_string()))?;
        atomic_create(
            &self.platform,
            &policy_root.join("config.yaml"),
            if mode == WorkspaceMode::Git {
                CONFIG_YAML
            } else {
                NON_GIT_CONFIG_YAML
            },
            0o644,
        )
        .map_err(|error| MachineError::initialization_conflict(canonical, error.to_string()))?;
        atomic_create(
            &self.platform,
            &policy_root.join(".gitignore"),
            PROJECT_IGNORE,
            0o644,
        )
        .map_err(|error| MachineError::initialization_conflict(canonical, error.to_string()))?;

        self.create_machine_layout(canonical, mode, workspace_id, state_root, baseline)
    }

    fn create_machine_layout(
        &self,
        canonical: &Path,
        mode: WorkspaceMode,
        workspace_id: &str,
        state_root: &Path,
        baseline: GitBaseline,
    ) -> Result<(), MachineError> {
        self.create_state_parents(state_root)?;
        for relative in [
            "specialist-policies",
            "runs",
            "runtime",
            "runtime/locks",
            "orchestration",
            "evidence",
            "cache",
        ] {
            create_directory(&state_root.join(relative), 0o700).map_err(|error| {
                MachineError::initialization_conflict(canonical, error.to_string())
            })?;
        }
        self.require_local_apfs(state_root)?;
        let record = WorkspaceRecord {
            schema_version: 1,
            workspace_id: workspace_id.to_owned(),
            canonical_path: LosslessPath::from_path(canonical),
            mode,
            initial_git_baseline: baseline,
            state_root_identity: file_identity(state_root).map_err(|error| {
                MachineError::initialization_conflict(canonical, error.to_string())
            })?,
            lock_root_identity: file_identity(&state_root.join("runtime/locks")).map_err(
                |error| MachineError::initialization_conflict(canonical, error.to_string()),
            )?,
        };
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|error| MachineError::initialization_conflict(canonical, error.to_string()))?;
        atomic_create(
            &self.platform,
            &state_root.join("workspace.json"),
            &bytes,
            0o600,
        )
        .map_err(|error| MachineError::initialization_conflict(canonical, error.to_string()))?;
        crate::writer::WriterStore::initialize_layout(
            state_root,
            workspace_id,
            self.platform.current_uid(),
        )?;
        sync_directory(state_root)
            .map_err(|error| MachineError::initialization_conflict(canonical, error.to_string()))
    }

    fn validate_portable_policy(
        &self,
        canonical: &Path,
        mode: WorkspaceMode,
        policy_root: &Path,
    ) -> Result<(), MachineError> {
        let metadata = fs::symlink_metadata(policy_root)
            .map_err(|error| MachineError::initialization_conflict(canonical, error.to_string()))?;
        if !metadata.file_type().is_dir() {
            return Err(MachineError::initialization_conflict(
                canonical,
                "portable policy root is not a directory",
            ));
        }
        let policy = parse_portable_policy(&policy_root.join("config.yaml"))?;
        if policy.schema_version != 1 || policy.mode != mode {
            return Err(MachineError::initialization_conflict(
                canonical,
                "portable policy mode or schema differs",
            ));
        }
        if read_bounded(&policy_root.join(".gitignore"), 4096)? != PROJECT_IGNORE {
            return Err(MachineError::initialization_conflict(
                canonical,
                "portable policy files are incompatible",
            ));
        }
        Ok(())
    }

    fn create_state_parents(&self, state_root: &Path) -> Result<(), MachineError> {
        let mut current = self.dolgorae_home_root.clone();
        if !current.exists() {
            let parent = current.parent().ok_or_else(|| {
                MachineError::runtime_path_invalid(&current, "Dolgorae home root has no parent")
            })?;
            self.require_local_apfs(parent)?;
            create_directory(&current, 0o700)
                .map_err(|error| MachineError::runtime_path_invalid(&current, error.to_string()))?;
        }
        verify_secure_directory(&current, self.platform.current_uid())?;
        current.push("workspaces");
        if !current.exists() {
            create_directory(&current, 0o700)
                .map_err(|error| MachineError::runtime_path_invalid(&current, error.to_string()))?;
        }
        verify_secure_directory(&current, self.platform.current_uid())?;
        create_directory(state_root, 0o700)
            .map_err(|error| MachineError::runtime_path_invalid(state_root, error.to_string()))?;
        Ok(())
    }

    fn validate_existing(
        &self,
        canonical: &Path,
        mode: WorkspaceMode,
        workspace_id: &str,
        policy_root: &Path,
        state_root: &Path,
        _validate_profiles: bool,
    ) -> Result<WorkspaceRecord, MachineError> {
        let policy_metadata = fs::symlink_metadata(policy_root).map_err(|error| {
            MachineError::initialization_conflict(
                canonical,
                format!("portable policy root: {error}"),
            )
        })?;
        if !policy_metadata.file_type().is_dir() {
            return Err(MachineError::initialization_conflict(
                canonical,
                "portable policy root is not a no-symlink directory",
            ));
        }
        for required in [
            policy_root.join("config.yaml"),
            policy_root.join(".gitignore"),
            state_root.join("workspace.json"),
            state_root.join("specialist-policies"),
            state_root.join("runs"),
            state_root.join("runtime"),
            state_root.join("orchestration"),
            state_root.join("evidence"),
            state_root.join("cache"),
        ] {
            if fs::symlink_metadata(&required).is_err() {
                return Err(MachineError::initialization_conflict(
                    canonical,
                    format!(
                        "partial workspace layout: {} is missing",
                        required.display()
                    ),
                ));
            }
        }
        let config_path = policy_root.join("config.yaml");
        let policy = parse_portable_policy(&config_path)?;
        if policy.schema_version != 1 || policy.mode != mode {
            return Err(MachineError::initialization_conflict(
                canonical,
                "portable policy mode or schema differs",
            ));
        }
        if read_bounded(&policy_root.join(".gitignore"), 4096)? != PROJECT_IGNORE {
            return Err(MachineError::initialization_conflict(
                canonical,
                "portable policy files are incompatible",
            ));
        }
        self.require_local_apfs(state_root)?;
        verify_secure_directory(state_root, self.platform.current_uid())?;
        for relative in [
            "specialist-policies",
            "runs",
            "runtime",
            "orchestration",
            "evidence",
            "cache",
        ] {
            verify_secure_directory(&state_root.join(relative), self.platform.current_uid())?;
        }
        verify_secure_file(
            &state_root.join("workspace.json"),
            self.platform.current_uid(),
        )?;
        let record_bytes = read_bounded(&state_root.join("workspace.json"), 1024 * 1024)?;
        let record_text = std::str::from_utf8(&record_bytes).map_err(|error| {
            MachineError::config_invalid(state_root.join("workspace.json"), error.to_string())
        })?;
        crate::jcs::parse(record_text).map_err(|error| {
            MachineError::config_invalid(state_root.join("workspace.json"), error.to_string())
        })?;
        let record: WorkspaceRecord = serde_json::from_slice(&record_bytes).map_err(|error| {
            MachineError::config_invalid(state_root.join("workspace.json"), error.to_string())
        })?;
        if record.schema_version != 1
            || record.workspace_id != workspace_id
            || record.canonical_path.to_path_buf()? != canonical
            || record.mode != mode
        {
            return Err(MachineError::initialization_conflict(
                canonical,
                "workspace record identity differs",
            ));
        }
        let observed_state = file_identity(state_root)
            .map_err(|error| MachineError::runtime_path_invalid(state_root, error.to_string()))?;
        if observed_state != record.state_root_identity {
            return Err(MachineError::runtime_path_collision(
                state_root,
                record.state_root_identity,
                observed_state,
            ));
        }
        let lock_root = state_root.join("runtime/locks");
        let observed_lock = file_identity(&lock_root).map_err(|error| {
            MachineError::runtime_path_collision_missing(
                &lock_root,
                record.lock_root_identity,
                error.to_string(),
            )
        })?;
        verify_secure_directory(&lock_root, self.platform.current_uid())?;
        if observed_lock != record.lock_root_identity {
            return Err(MachineError::runtime_path_collision(
                &lock_root,
                record.lock_root_identity,
                observed_lock,
            ));
        }
        Ok(record)
    }
}

fn reinitialization_error(canonical: &Path, error: MachineError) -> MachineError {
    if matches!(
        error.code.as_str(),
        "CONFIG_INVALID" | "PROFILE_CONFIG_INVALID" | "WORKSPACE_INITIALIZATION_CONFLICT"
    ) {
        MachineError::initialization_conflict(
            canonical,
            format!("{}: {}", error.code, error.message),
        )
    } else {
        error
    }
}

fn view_from_record(record: WorkspaceRecord, created: bool) -> WorkspaceView {
    WorkspaceView {
        workspace_id: record.workspace_id,
        canonical_path: record.canonical_path,
        mode: record.mode,
        created,
    }
}

fn git_arguments(root: &Path, command: &[&str]) -> Vec<OsString> {
    let mut arguments = vec![
        OsString::from("-c"),
        OsString::from("core.quotePath=true"),
        OsString::from("-C"),
        root.as_os_str().to_owned(),
    ];
    arguments.extend(command.iter().map(OsString::from));
    arguments
}

pub fn decode_git_toplevel(stdout: &[u8]) -> Result<OsString, String> {
    let body = stdout
        .strip_suffix(b"\n")
        .ok_or_else(|| "Git top-level output lacks exactly one final LF".to_owned())?;
    if body.is_empty() || body.contains(&b'\n') || body.contains(&0) {
        return Err("Git top-level output is empty, multiline, or contains NUL".to_owned());
    }
    if body[0] != b'"' {
        return Ok(OsString::from_vec(body.to_vec()));
    }
    if body.len() < 2 || body.last() != Some(&b'"') {
        return Err("invalid Git C-style path quoting".to_owned());
    }
    let mut decoded = Vec::with_capacity(body.len());
    let mut index = 1;
    while index < body.len() - 1 {
        let byte = body[index];
        if byte != b'\\' {
            if byte == b'"' {
                return Err("unescaped quote in Git path".to_owned());
            }
            decoded.push(byte);
            index += 1;
            continue;
        }
        index += 1;
        let escaped = *body
            .get(index)
            .ok_or_else(|| "truncated Git path escape".to_owned())?;
        match escaped {
            b'a' => decoded.push(0x07),
            b'b' => decoded.push(0x08),
            b't' => decoded.push(b'\t'),
            b'n' => decoded.push(b'\n'),
            b'v' => decoded.push(0x0b),
            b'f' => decoded.push(0x0c),
            b'r' => decoded.push(b'\r'),
            b'\\' | b'"' => decoded.push(escaped),
            b'0'..=b'7' => {
                if index + 2 >= body.len() - 1 {
                    return Err("truncated Git octal path escape".to_owned());
                }
                let octets = &body[index..=index + 2];
                if !octets.iter().all(|value| matches!(value, b'0'..=b'7')) {
                    return Err("invalid Git octal path escape".to_owned());
                }
                let value = u16::from(octets[0] - b'0') * 64
                    + u16::from(octets[1] - b'0') * 8
                    + u16::from(octets[2] - b'0');
                if value == 0 || value > u16::from(u8::MAX) {
                    return Err("Git path contains NUL".to_owned());
                }
                decoded.push(u8::try_from(value).expect("range checked"));
                index += 2;
            }
            _ => return Err("unsupported Git path escape".to_owned()),
        }
        index += 1;
    }
    if decoded.contains(&0) {
        return Err("Git path contains NUL".to_owned());
    }
    Ok(OsString::from_vec(decoded))
}

fn parse_porcelain_v2_z(
    bytes: &[u8],
) -> Result<(Vec<LosslessPath>, Vec<LosslessPath>), MachineError> {
    let mut tracked = BTreeSet::<Vec<u8>>::new();
    let mut untracked = BTreeSet::<Vec<u8>>::new();
    let records = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.is_empty() {
            index += 1;
            continue;
        }
        match record.first() {
            Some(b'?') if record.get(1) == Some(&b' ') => {
                untracked.insert(record[2..].to_vec());
            }
            Some(b'1') => {
                let path = record
                    .splitn(9, |byte| *byte == b' ')
                    .nth(8)
                    .ok_or_else(|| {
                        MachineError::initialization_conflict("git", "invalid porcelain v2 record")
                    })?;
                tracked.insert(path.to_vec());
            }
            Some(b'u') => {
                let path = record
                    .splitn(11, |byte| *byte == b' ')
                    .nth(10)
                    .ok_or_else(|| {
                        MachineError::initialization_conflict(
                            "git",
                            "invalid porcelain v2 unmerged record",
                        )
                    })?;
                tracked.insert(path.to_vec());
            }
            Some(b'2') => {
                let path = record
                    .splitn(10, |byte| *byte == b' ')
                    .nth(9)
                    .ok_or_else(|| {
                        MachineError::initialization_conflict(
                            "git",
                            "invalid porcelain v2 rename record",
                        )
                    })?;
                tracked.insert(path.to_vec());
                index += 1;
                if index >= records.len() {
                    return Err(MachineError::initialization_conflict(
                        "git",
                        "missing porcelain v2 rename source",
                    ));
                }
            }
            Some(b'#') | Some(b'!') => {}
            _ => {
                return Err(MachineError::initialization_conflict(
                    "git",
                    "unknown porcelain v2 record",
                ));
            }
        }
        index += 1;
    }
    Ok((
        tracked.into_iter().map(lossless_relative).collect(),
        untracked.into_iter().map(lossless_relative).collect(),
    ))
}

fn lossless_relative(bytes: Vec<u8>) -> LosslessPath {
    LosslessPath::from_path(Path::new(&OsString::from_vec(bytes)))
}

pub fn normalize_data_volume_alias(
    path: PathBuf,
    mut identity: impl FnMut(&Path) -> Option<FileIdentity>,
) -> Result<PathBuf, MachineError> {
    let prefix = Path::new("/System/Volumes/Data");
    if path != prefix && !path.starts_with(prefix) {
        return Ok(path);
    }
    let suffix = path.strip_prefix(prefix).expect("prefix checked");
    let candidate = if suffix.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        Path::new("/").join(suffix)
    };
    let source_identity = identity(&path);
    if source_identity.is_some() && source_identity == identity(&candidate) {
        Ok(candidate)
    } else {
        Ok(path)
    }
}

#[must_use]
pub fn workspace_id(canonical: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"dolgorae-workspace-v1\0");
    hasher.update(canonical.as_os_str().as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn parse_portable_policy(path: &Path) -> Result<PortablePolicy, MachineError> {
    let bytes = read_bounded(path, 4096)?;
    serde_yaml_ng::from_slice(&bytes)
        .map_err(|error| MachineError::config_invalid(path, error.to_string()))
}

pub fn parse_local_profiles(path: &Path) -> Result<LocalProfileRegistry, MachineError> {
    let bytes = read_bounded(path, 1024 * 1024)?;
    let registry: LocalProfileRegistry = serde_yaml_ng::from_slice(&bytes)
        .map_err(|error| MachineError::profile_config_invalid(path, error.to_string()))?;
    validate_runtime_profiles(path, registry.schema_version, &registry.profiles)?;
    Ok(registry)
}

pub(crate) fn validate_runtime_profiles(
    path: &Path,
    schema_version: u32,
    profiles: &BTreeMap<String, RuntimeProfile>,
) -> Result<(), MachineError> {
    if schema_version != 1 {
        return Err(MachineError::profile_config_invalid(
            path,
            "unsupported schema_version",
        ));
    }
    for (name, profile) in profiles {
        if name.is_empty()
            || profile.argv.is_empty()
            || profile.argv.iter().any(String::is_empty)
            || !Path::new(&profile.argv[0]).is_absolute()
            || !Path::new(&profile.codex_home).is_absolute()
        {
            return Err(MachineError::profile_config_invalid(
                path,
                format!("profile {name:?} has an invalid name, argv, or CODEX_HOME"),
            ));
        }
        for required in ["PATH", "LANG", "LC_ALL"] {
            if profile
                .environment
                .get(required)
                .is_none_or(String::is_empty)
            {
                return Err(MachineError::profile_config_invalid(
                    path,
                    format!("profile {name:?} lacks environment {required}"),
                ));
            }
        }
        if profile.environment.iter().any(|(key, value)| {
            key.is_empty()
                || value.is_empty()
                || matches!(
                    key.as_str(),
                    "CODEX_HOME" | "HOME" | "USER" | "LOGNAME" | "SHELL" | "TMPDIR"
                )
                || key.starts_with("DOLGORAE_")
        }) {
            return Err(MachineError::profile_config_invalid(
                path,
                format!("profile {name:?} contains a reserved environment name"),
            ));
        }
        validate_profile_path(path, name, &profile.environment["PATH"])?;
        validate_profile_argv(path, name, &profile.argv)?;
    }
    Ok(())
}

fn validate_profile_path(
    registry_path: &Path,
    profile_name: &str,
    value: &str,
) -> Result<(), MachineError> {
    let mut seen = BTreeSet::new();
    for component in value.split(':') {
        let path = Path::new(component);
        let existing_directory = fs::metadata(path).is_ok_and(|metadata| metadata.is_dir());
        if component.is_empty()
            || !path.is_absolute()
            || !seen.insert(component)
            || !existing_directory
        {
            return Err(MachineError::profile_config_invalid(
                registry_path,
                format!(
                    "profile {profile_name:?} PATH must contain unique existing absolute directories"
                ),
            ));
        }
    }
    Ok(())
}

fn validate_profile_argv(
    registry_path: &Path,
    profile_name: &str,
    argv: &[String],
) -> Result<(), MachineError> {
    validate_direct_executable(registry_path, profile_name, Path::new(&argv[0]))?;

    let mut index = 1;
    while index < argv.len() {
        let option = argv[index].as_str();
        if option.contains('=') {
            return Err(MachineError::profile_config_invalid(
                registry_path,
                format!("profile {profile_name:?} uses a forbidden equals-form option"),
            ));
        }
        match option {
            "--strict-config" => index += 1,
            "--profile" | "--enable" | "--disable" => {
                let value = argv
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.starts_with('-'));
                let Some(value) = value else {
                    return Err(MachineError::profile_config_invalid(
                        registry_path,
                        format!("profile {profile_name:?} has a missing value for {option}"),
                    ));
                };
                if matches!(option, "--enable" | "--disable") && value == "multi_agent" {
                    return Err(MachineError::profile_config_invalid(
                        registry_path,
                        format!("profile {profile_name:?} contains reserved multi_agent policy"),
                    ));
                }
                index += 2;
            }
            _ => {
                return Err(MachineError::profile_config_invalid(
                    registry_path,
                    format!("profile {profile_name:?} contains unsupported argv token {option:?}"),
                ));
            }
        }
    }
    Ok(())
}

/// The Mach-O and universal-binary magics a directly executable native image
/// starts with, in both byte orders and both widths.
const NATIVE_IMAGE_MAGICS: [[u8; 4]; 6] = [
    [0xfe, 0xed, 0xfa, 0xce],
    [0xce, 0xfa, 0xed, 0xfe],
    [0xfe, 0xed, 0xfa, 0xcf],
    [0xcf, 0xfa, 0xed, 0xfe],
    [0xca, 0xfe, 0xba, 0xbe],
    [0xbe, 0xba, 0xfe, 0xca],
];

/// Enforces the direct-executable boundary on `argv[0]`.
///
/// SPEC-013 requires `argv[0]` to be "an absolute regular Codex executable"
/// and says v1 "rejects shell interpreters, arbitrary wrappers"; ADR-016
/// rejects the wrapper command outright because "its environment mutation,
/// descendant ownership, and launch identity cannot be deterministic in v1".
/// Checking only that the file exists, carries an executable bit, and is
/// *named* `codex` enforces none of that: a two-line `#!/bin/sh` script named
/// `codex`, or a symlink named `codex` pointing at `/usr/bin/env`, satisfies
/// every one of those conditions and is exactly the interpreter-and-wrapper
/// launch the contract excludes. The consequence is not cosmetic — SPEC-013's
/// spawn-image rule ("because v1 forbids wrappers, this image MUST be the
/// configured Codex executable") is unprovable the moment the kernel execs an
/// interpreter instead of the recorded image, and the launched process's
/// environment and process group belong to the wrapper rather than to the
/// launch contract.
///
/// So the boundary is the file itself:
///
/// * the path and the target it resolves to are both named `codex`, which is
///   what stops a `codex`-named symlink from pointing at an arbitrary program;
/// * the target is a regular file with an executable bit and no setuid or
///   setgid bit, because a privileged image is a different launch identity
///   than the one recorded;
/// * the first bytes are a native Mach-O or universal-binary magic and are not
///   a `#!` interpreter line, which is what separates a directly executable
///   image from a script the kernel would hand to an interpreter.
///
/// This is a check on the launch boundary, not a proof of provenance: a
/// compiled program that merely re-execs something else is still a native
/// image. Proving the running image is the recorded one is the spawn-image and
/// process-census work TASK-020 owns; this closes the boundary the registry
/// itself is responsible for.
fn validate_direct_executable(
    registry_path: &Path,
    profile_name: &str,
    executable: &Path,
) -> Result<(), MachineError> {
    let invalid = |reason: &str| {
        MachineError::profile_config_invalid(
            registry_path,
            format!("profile {profile_name:?} argv[0] is not a direct Codex executable: {reason}"),
        )
    };
    let codex = Some(std::ffi::OsStr::new("codex"));
    if executable.file_name() != codex {
        return Err(invalid("the configured path is not named codex"));
    }
    let resolved = fs::canonicalize(executable).map_err(|error| {
        MachineError::profile_config_invalid(
            registry_path,
            format!("profile {profile_name:?} executable: {error}"),
        )
    })?;
    if resolved.file_name() != codex {
        return Err(invalid(
            "the configured path resolves to a differently named program",
        ));
    }
    let metadata = fs::metadata(&resolved).map_err(|error| {
        MachineError::profile_config_invalid(
            registry_path,
            format!("profile {profile_name:?} executable: {error}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(invalid("the resolved image is not a regular file"));
    }
    if metadata.mode() & 0o111 == 0 {
        return Err(invalid("the resolved image carries no executable bit"));
    }
    if metadata.mode() & 0o6000 != 0 {
        return Err(invalid("the resolved image is setuid or setgid"));
    }
    let mut header = [0_u8; 4];
    let read = File::open(&resolved)
        .and_then(|mut file| file.read(&mut header))
        .map_err(|error| {
            MachineError::profile_config_invalid(
                registry_path,
                format!("profile {profile_name:?} executable: {error}"),
            )
        })?;
    if read >= 2 && &header[..2] == b"#!" {
        return Err(invalid(
            "the resolved image is an interpreter script, not a direct executable",
        ));
    }
    if read < 4 || !NATIVE_IMAGE_MAGICS.contains(&header) {
        return Err(invalid("the resolved image is not a native executable"));
    }
    Ok(())
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, MachineError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MachineError::config_invalid(path, error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.len() > maximum as u64 {
        return Err(MachineError::config_invalid(
            path,
            "file must be a bounded no-symlink regular file",
        ));
    }
    fs::read(path).map_err(|error| MachineError::config_invalid(path, error.to_string()))
}

pub(crate) fn create_directory(path: &Path, mode: u32) -> Result<(), std::io::Error> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(mode);
    builder.create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

pub(crate) fn atomic_create(
    platform: &impl WorkspacePlatform,
    destination: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), std::io::Error> {
    let parent = destination.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let nonce = uuid::Uuid::now_v7();
    let temporary = parent.join(format!(".dolgorae-tmp-{nonce}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    file.write_all(bytes)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    file.sync_all()?;
    if let Err(error) = platform.rename_exclusive(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sync_directory(parent)
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    File::open(path)?.sync_all()
}

fn file_identity(path: &Path) -> Result<FileIdentity, std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(crate) fn verify_secure_directory(path: &Path, uid: u32) -> Result<(), MachineError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MachineError::runtime_path_invalid(path, error.to_string()))?;
    if !metadata.file_type().is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        return Err(MachineError::runtime_path_invalid(
            path,
            "directory must be current-uid-owned mode 0700 without symlink traversal",
        ));
    }
    Ok(())
}

pub(crate) fn open_secure_file_if_present(
    path: &Path,
    uid: u32,
) -> Result<Option<File>, MachineError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MachineError::runtime_path_invalid(path, error.to_string())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| MachineError::runtime_path_invalid(path, error.to_string()))?;
    if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o600 {
        return Err(MachineError::runtime_path_invalid(
            path,
            "file must be current-uid-owned mode 0600 without symlink traversal",
        ));
    }
    Ok(Some(file))
}

pub(crate) fn verify_secure_file(path: &Path, uid: u32) -> Result<(), MachineError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MachineError::runtime_path_invalid(path, error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o600
    {
        return Err(MachineError::runtime_path_invalid(
            path,
            "file must be current-uid-owned mode 0600 without symlink traversal",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_admission_precedes_shared_state_preparation() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-init-admission-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("workspace");
        let policy = workspace.join(".dolgorae");
        fs::create_dir_all(&policy).unwrap();
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        fs::write(policy.join("config.yaml"), "schema_version: 1\nmode: git\n").unwrap();
        fs::write(policy.join(".gitignore"), "/exports/\n").unwrap();
        let state = home.join(".dolgorae");
        let service =
            WorkspaceService::new(SystemWorkspacePlatform, SystemGitRunner, state.clone());
        let error = service
            .initialize_with_preparation(Some(&workspace), WorkspaceMode::NonGit, || {
                panic!("incompatible policy reached shared state preparation")
            })
            .unwrap_err();
        assert_eq!(error.code, "WORKSPACE_INITIALIZATION_CONFLICT");
        assert!(!state.exists());
        fs::write(
            policy.join("config.yaml"),
            "schema_version: 1\nmode: non_git\n",
        )
        .unwrap();
        let before = fs::read(policy.join("config.yaml")).unwrap();
        let called = std::cell::Cell::new(false);
        let error = service
            .initialize_with_preparation(Some(&workspace), WorkspaceMode::NonGit, || {
                called.set(true);
                Err(MachineError::initialization_conflict(
                    &workspace,
                    "shared preparation failed",
                ))
            })
            .unwrap_err();
        assert!(called.get());
        assert_eq!(error.code, "WORKSPACE_INITIALIZATION_CONFLICT");
        assert!(!state.exists());
        assert_eq!(fs::read(policy.join("config.yaml")).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_quoted_paths_decode_losslessly() {
        assert_eq!(
            decode_git_toplevel(b"\"/tmp/a\\tb\\303\\251\"\n")
                .unwrap()
                .as_bytes(),
            b"/tmp/a\tb\xc3\xa9"
        );
        assert!(decode_git_toplevel(b"/tmp/a\nextra\n").is_err());
        assert!(decode_git_toplevel(b"\"/tmp/\\000\"\n").is_err());
    }

    #[test]
    fn workspace_digest_uses_domain_and_raw_path_bytes() {
        assert_eq!(workspace_id(Path::new("/tmp/example")).len(), 64);
        assert_ne!(
            workspace_id(Path::new("/tmp/example")),
            workspace_id(Path::new("/tmp/Example"))
        );
    }

    #[test]
    fn registered_worktree_identity_accepts_a_symlinked_parent_spelling() {
        let root = std::env::temp_dir().join(format!(
            "dolgorae-worktree-identity-{}",
            uuid::Uuid::now_v7()
        ));
        let canonical = root.join("repository");
        let actual_parent = root.join("actual");
        fs::create_dir_all(&canonical).unwrap();
        fs::create_dir(&actual_parent).unwrap();
        let git = |arguments: &[&str]| {
            assert!(
                Command::new("/usr/bin/git")
                    .arg("-C")
                    .arg(&canonical)
                    .args(arguments)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@dolgorae.local"]);
        git(&["config", "user.name", "Dolgorae Test"]);
        fs::write(canonical.join("tracked.txt"), "baseline\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "baseline"]);
        let actual_worktree = actual_parent.join("worktree");
        add_detached_git_worktree(&canonical, &actual_worktree).unwrap();
        let alias_parent = root.join("alias");
        std::os::unix::fs::symlink(&actual_parent, &alias_parent).unwrap();
        assert!(is_registered_git_worktree(
            &canonical,
            &alias_parent.join("worktree")
        ));
        remove_git_worktree(&canonical, &actual_worktree).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn run_baseline_captures_the_current_tree_not_the_initialization_snapshot() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-run-baseline-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let git = |arguments: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(arguments)
                .status()
                .unwrap();
            assert!(status.success(), "git {arguments:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@dolgorae.local"]);
        git(&["config", "user.name", "Dolgorae Test"]);
        fs::write(root.join("tracked.txt"), "initial\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "baseline"]);
        fs::write(root.join("tracked.txt"), "changed\n").unwrap();
        fs::write(root.join("untracked.txt"), "new\n").unwrap();

        let service = WorkspaceService::new(
            SystemWorkspacePlatform,
            SystemGitRunner,
            root.join("dolgorae-home"),
        );
        let view = WorkspaceView {
            workspace_id: workspace_id(&root),
            canonical_path: LosslessPath::from_path(&root),
            mode: WorkspaceMode::Git,
            created: false,
        };
        let baseline = service.capture_run_baseline(&view).unwrap();
        assert!(baseline.head.is_some());
        assert_eq!(
            baseline.tracked_changes,
            vec![LosslessPath::from_path(Path::new("tracked.txt"))]
        );
        assert_eq!(
            baseline.untracked_paths,
            vec![LosslessPath::from_path(Path::new("untracked.txt"))]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn firmlink_substitution_requires_matching_identity() {
        let identity = FileIdentity {
            device: 1,
            inode: 2,
        };
        let path = PathBuf::from("/System/Volumes/Data/Users/test/project");
        let normalized = normalize_data_volume_alias(path.clone(), |_| Some(identity)).unwrap();
        assert_eq!(normalized, Path::new("/Users/test/project"));
        let distinct = normalize_data_volume_alias(path.clone(), |candidate| {
            Some(FileIdentity {
                device: 1,
                inode: u64::from(candidate.starts_with("/System/Volumes/Data")) + 2,
            })
        })
        .unwrap();
        assert_eq!(distinct, path);
    }

    #[test]
    fn strict_yaml_rejects_unknown_and_duplicate_keys() {
        assert!(
            serde_yaml_ng::from_str::<PortablePolicy>(
                "schema_version: 1\nmode: git\nextra: true\n"
            )
            .is_err()
        );
        assert!(
            serde_yaml_ng::from_str::<PortablePolicy>(
                "schema_version: 1\nmode: git\nmode: non_git\n"
            )
            .is_err()
        );
        assert!(
            serde_yaml_ng::from_str::<LocalProfileRegistry>(
                "schema_version: 1\nprofiles: {}\nextra: true\n"
            )
            .is_err()
        );
    }

    /// SPEC-013 requires `argv[0]` to be a direct Codex executable and
    /// "rejects shell interpreters, arbitrary wrappers"; ADR-016 excludes the
    /// wrapper command because its environment, descendants, and launch
    /// identity are not deterministic. Existence plus an executable bit plus
    /// the name `codex` enforced none of that.
    #[test]
    fn argv0_must_be_a_direct_native_codex_image_not_a_script_or_a_wrapper() {
        let root =
            std::env::temp_dir().join(format!("dolgorae-direct-argv0-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let registry = root.join("local.yaml");
        let check = |path: &Path| validate_direct_executable(&registry, "default", path);

        // A native image is what the contract asks for. /bin/sh is one, so a
        // copy of it stands in for a real Codex binary here; what this proves
        // is the file-format boundary, not provenance.
        let native = root.join("codex");
        fs::copy("/bin/sh", &native).unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o755)).unwrap();
        check(&native).expect("a native executable named codex is accepted");

        // A shell script named codex: the kernel execs an interpreter, so the
        // spawn image can never be the configured executable.
        let script = root.join("script");
        fs::create_dir(&script).unwrap();
        let scripted = script.join("codex");
        fs::write(&scripted, b"#!/bin/sh\nexec /bin/sh \"$@\"\n").unwrap();
        fs::set_permissions(&scripted, fs::Permissions::from_mode(0o755)).unwrap();
        let error = check(&scripted).unwrap_err();
        assert_eq!(error.code, "PROFILE_CONFIG_INVALID");
        assert!(
            error.details["reason"]
                .as_str()
                .unwrap()
                .contains("interpreter script"),
            "{error:?}"
        );

        // A symlink named codex pointing at an arbitrary program: the name
        // check alone passed this, and the launched program was /bin/echo.
        let wrapper = root.join("wrapper");
        fs::create_dir(&wrapper).unwrap();
        let linked = wrapper.join("codex");
        std::os::unix::fs::symlink("/bin/echo", &linked).unwrap();
        let error = check(&linked).unwrap_err();
        assert!(
            error.details["reason"]
                .as_str()
                .unwrap()
                .contains("differently named program"),
            "{error:?}"
        );

        // A symlink to a native codex image is still direct: the resolved
        // program is the same executable under the same name.
        let aliased = wrapper.join("alias");
        fs::create_dir(&aliased).unwrap();
        std::os::unix::fs::symlink(&native, aliased.join("codex")).unwrap();
        check(&aliased.join("codex")).expect("a symlink to the codex image stays direct");

        // Text with no shebang is still not a native image.
        let text = root.join("text");
        fs::create_dir(&text).unwrap();
        let texted = text.join("codex");
        fs::write(&texted, b"not an executable at all\n").unwrap();
        fs::set_permissions(&texted, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            check(&texted).unwrap_err().details["reason"]
                .as_str()
                .unwrap()
                .contains("not a native executable")
        );

        // A native image with no executable bit, and one that is setuid.
        let bits = root.join("bits");
        fs::create_dir(&bits).unwrap();
        let unexecutable = bits.join("codex");
        fs::copy("/bin/sh", &unexecutable).unwrap();
        fs::set_permissions(&unexecutable, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            check(&unexecutable).unwrap_err().details["reason"]
                .as_str()
                .unwrap()
                .contains("no executable bit")
        );
        fs::set_permissions(&unexecutable, fs::Permissions::from_mode(0o4755)).unwrap();
        assert!(
            check(&unexecutable).unwrap_err().details["reason"]
                .as_str()
                .unwrap()
                .contains("setuid or setgid")
        );

        // A directory named codex, and a path that is not named codex at all.
        let directory = root.join("directory");
        fs::create_dir_all(directory.join("codex")).unwrap();
        assert!(check(&directory.join("codex")).is_err());
        assert!(check(Path::new("/bin/sh")).is_err());

        fs::remove_dir_all(&root).unwrap();
    }
}
