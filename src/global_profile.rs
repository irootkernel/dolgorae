//! Inactive successor contract for global Codex Profiles.
//!
//! TASK-036 deliberately does not connect this module to production command
//! dispatch. TASK-038 activates it only after every Run and Specialist
//! consumer is ready for the same hard-cut home generation.

use crate::darwin::DarwinSystem;
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::profile::{ProfileOperation, validate_arguments};
use crate::workspace::{
    RuntimeProfile, SystemWorkspacePlatform, atomic_create, create_directory, sync_directory,
    validate_runtime_profiles, verify_secure_directory, verify_secure_file,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const HOME_STATE_BYTES: &[u8] =
    b"{\n  \"schema_version\": 1,\n  \"state_generation\": \"global-profile-v1\"\n}\n";
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeState {
    pub schema_version: u32,
    pub state_generation: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeGenerationStatus {
    Uninitialized,
    GlobalProfileV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalProfileRegistry {
    pub schema_version: u32,
    pub profiles: BTreeMap<String, RuntimeProfile>,
}

pub struct GlobalProfileStore {
    root: PathBuf,
}

impl GlobalProfileStore {
    #[must_use]
    pub fn new(home: &DolgoraeHome) -> Self {
        Self {
            root: home.root().to_path_buf(),
        }
    }

    pub fn load(&self) -> Result<GlobalProfileRegistry, MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        load_registry(&self.root)
    }

    pub fn add(
        &self,
        name: String,
        profile: RuntimeProfile,
    ) -> Result<GlobalProfileRegistry, MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        let mut registry = load_registry_or_empty(&self.root)?;
        if registry.profiles.contains_key(&name) {
            return Err(MachineError::new(
                "PROFILE_ALREADY_EXISTS",
                "profile already exists",
                false,
                json!({"profile": name}),
            ));
        }
        registry.profiles.insert(name, profile);
        validate_runtime_profiles(
            &self.root.join("profiles.yaml"),
            registry.schema_version,
            &registry.profiles,
        )?;
        store_registry(&self.root, &registry)?;
        Ok(registry)
    }

    pub fn remove(&self, name: &str) -> Result<GlobalProfileRegistry, MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        let mut registry = load_registry(&self.root)?;
        if registry.profiles.remove(name).is_none() {
            return Err(MachineError::new(
                "PROFILE_NOT_FOUND",
                "profile was not found",
                false,
                json!({"profile": name}),
            ));
        }
        store_registry(&self.root, &registry)?;
        Ok(registry)
    }
}

pub fn validate_post_cut_arguments(
    operation: ProfileOperation,
    arguments: &[OsString],
) -> Result<(), MachineError> {
    if arguments.iter().any(|argument| {
        argument
            .to_str()
            .is_some_and(|value| value == "--workspace" || value.starts_with("--workspace="))
    }) {
        return Err(MachineError::invalid_argument(
            "--workspace",
            "global Profile commands do not accept a workspace",
        ));
    }
    validate_arguments(operation, arguments)
}

pub fn inspect_generation(home: &DolgoraeHome) -> Result<HomeGenerationStatus, MachineError> {
    inspect_root(home.root())
}

pub fn initialize_generation(home: &DolgoraeHome) -> Result<bool, MachineError> {
    let root = home.root();
    if !root.exists() {
        create_directory(root, 0o700)
            .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
    }
    verify_secure_directory(root, DarwinSystem.current_uid())?;
    let directory = File::open(root)
        .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
    DarwinSystem
        .lock_exclusive(&directory)
        .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
    let created = match inspect_root(root)? {
        HomeGenerationStatus::GlobalProfileV1 => false,
        HomeGenerationStatus::Uninitialized => {
            atomic_create(
                &SystemWorkspacePlatform,
                &root.join("state.json"),
                HOME_STATE_BYTES,
                0o600,
            )
            .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
            if inspect_root(root)? != HomeGenerationStatus::GlobalProfileV1 {
                return Err(MachineError::new(
                    "INTERNAL_ERROR",
                    "new Dolgorae home generation did not validate",
                    false,
                    json!({"invariant": "home_generation_readback"}),
                ));
            }
            true
        }
    };
    if !root.join("profiles.yaml").exists() {
        store_registry(
            root,
            &GlobalProfileRegistry {
                schema_version: 1,
                profiles: BTreeMap::new(),
            },
        )?;
    }
    Ok(created)
}

fn inspect_root(root: &Path) -> Result<HomeGenerationStatus, MachineError> {
    if !root.exists() {
        return Ok(HomeGenerationStatus::Uninitialized);
    }
    verify_secure_directory(root, DarwinSystem.current_uid())?;
    let marker = root.join("state.json");
    if !marker.exists() {
        let empty = fs::read_dir(root)
            .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?
            .next()
            .is_none();
        return if empty {
            Ok(HomeGenerationStatus::Uninitialized)
        } else {
            Err(legacy_state_unsupported(root, "unmarked_nonempty"))
        };
    }
    verify_secure_file(&marker, DarwinSystem.current_uid())?;
    let bytes = fs::read(&marker)
        .map_err(|error| MachineError::runtime_path_invalid(&marker, error.to_string()))?;
    if bytes.len() > 4096 {
        return Err(legacy_state_unsupported(root, "malformed_marker"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
    crate::jcs::parse(text).map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
    let state: HomeState = serde_json::from_slice(&bytes)
        .map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
    if state.schema_version != 1 || state.state_generation != "global-profile-v1" {
        return Err(legacy_state_unsupported(root, "unsupported_generation"));
    }
    if contains_legacy_registry(root)? {
        return Err(legacy_state_unsupported(root, "mixed_generation"));
    }
    Ok(HomeGenerationStatus::GlobalProfileV1)
}

fn contains_legacy_registry(root: &Path) -> Result<bool, MachineError> {
    let workspaces = root.join("workspaces");
    if !workspaces.exists() {
        return Ok(false);
    }
    let entries = fs::read_dir(&workspaces)
        .map_err(|error| MachineError::runtime_path_invalid(&workspaces, error.to_string()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| MachineError::runtime_path_invalid(&workspaces, error.to_string()))?;
        let metadata = entry
            .file_type()
            .map_err(|error| MachineError::runtime_path_invalid(entry.path(), error.to_string()))?;
        if metadata.is_symlink() || !metadata.is_dir() {
            return Err(legacy_state_unsupported(root, "partial_generation"));
        }
        if entry.path().join("local.yaml").exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn lock_initialized_home(root: &Path) -> Result<File, MachineError> {
    verify_secure_directory(root, DarwinSystem.current_uid())?;
    let directory = File::open(root)
        .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
    DarwinSystem
        .lock_exclusive(&directory)
        .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
    if inspect_root(root)? != HomeGenerationStatus::GlobalProfileV1 {
        return Err(legacy_state_unsupported(root, "partial_generation"));
    }
    Ok(directory)
}

fn load_registry(root: &Path) -> Result<GlobalProfileRegistry, MachineError> {
    let path = root.join("profiles.yaml");
    verify_secure_file(&path, DarwinSystem.current_uid())?;
    let bytes = fs::read(&path)
        .map_err(|error| MachineError::profile_config_invalid(&path, error.to_string()))?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(MachineError::profile_config_invalid(
            &path,
            "profile registry exceeds 1 MiB",
        ));
    }
    let registry: GlobalProfileRegistry = serde_yaml_ng::from_slice(&bytes)
        .map_err(|error| MachineError::profile_config_invalid(&path, error.to_string()))?;
    validate_runtime_profiles(&path, registry.schema_version, &registry.profiles)?;
    Ok(registry)
}

fn load_registry_or_empty(root: &Path) -> Result<GlobalProfileRegistry, MachineError> {
    if root.join("profiles.yaml").exists() {
        load_registry(root)
    } else {
        Ok(GlobalProfileRegistry {
            schema_version: 1,
            profiles: BTreeMap::new(),
        })
    }
}

fn store_registry(root: &Path, registry: &GlobalProfileRegistry) -> Result<(), MachineError> {
    let path = root.join("profiles.yaml");
    let bytes = serde_yaml_ng::to_string(registry)
        .map_err(|error| MachineError::profile_config_invalid(&path, error.to_string()))?
        .into_bytes();
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(MachineError::profile_config_invalid(
            &path,
            "profile registry exceeds 1 MiB",
        ));
    }
    let temporary = root.join(format!(".profiles-{}.yaml", Uuid::now_v7()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|error| MachineError::runtime_path_invalid(&temporary, error.to_string()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| MachineError::runtime_path_invalid(&temporary, error.to_string()))?;
    if let Err(error) = parse_registry_bytes(&temporary, &bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, &path)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .and_then(|()| sync_directory(root))
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    Ok(())
}

fn parse_registry_bytes(path: &Path, bytes: &[u8]) -> Result<GlobalProfileRegistry, MachineError> {
    let registry: GlobalProfileRegistry = serde_yaml_ng::from_slice(bytes)
        .map_err(|error| MachineError::profile_config_invalid(path, error.to_string()))?;
    validate_runtime_profiles(path, registry.schema_version, &registry.profiles)?;
    Ok(registry)
}

fn legacy_state_unsupported(root: &Path, classification: &'static str) -> MachineError {
    MachineError::new(
        "LEGACY_STATE_UNSUPPORTED",
        "Dolgorae home state is not compatible with the global Profile generation",
        false,
        json!({"home": root, "classification": classification}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn test_home() -> (PathBuf, DolgoraeHome) {
        let parent =
            std::env::temp_dir().join(format!("dolgorae-global-profile-{}", Uuid::now_v7()));
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        let home = DolgoraeHome::from_canonical_home(parent.clone()).unwrap();
        (parent, home)
    }

    fn valid_profile(parent: &Path) -> RuntimeProfile {
        let executable = parent.join("codex");
        if !executable.exists() {
            fs::write(&executable, [0xcf, 0xfa, 0xed, 0xfe]).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        }
        RuntimeProfile {
            argv: vec![executable.to_string_lossy().into_owned()],
            codex_home: parent.to_string_lossy().into_owned(),
            environment: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("LANG".to_owned(), "C.UTF-8".to_owned()),
                ("LC_ALL".to_owned(), "C.UTF-8".to_owned()),
            ]),
            native_subagents: crate::workspace::NativeSubagents::Enabled,
        }
    }

    #[test]
    fn initializes_and_reopens_exact_generation() {
        let (parent, home) = test_home();
        assert_eq!(
            inspect_generation(&home).unwrap(),
            HomeGenerationStatus::Uninitialized
        );
        assert!(initialize_generation(&home).unwrap());
        assert!(!initialize_generation(&home).unwrap());
        assert_eq!(
            inspect_generation(&home).unwrap(),
            HomeGenerationStatus::GlobalProfileV1
        );
        assert_eq!(
            fs::metadata(home.root().join("state.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            GlobalProfileStore::new(&home)
                .load()
                .unwrap()
                .profiles
                .len(),
            0
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn rejects_unmarked_and_mixed_homes_without_changing_bytes() {
        let (parent, home) = test_home();
        fs::create_dir(home.root()).unwrap();
        fs::set_permissions(home.root(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(home.root().join("legacy"), b"unchanged").unwrap();
        let before = fs::read(home.root().join("legacy")).unwrap();
        assert_eq!(
            inspect_generation(&home).unwrap_err().code,
            "LEGACY_STATE_UNSUPPORTED"
        );
        assert_eq!(fs::read(home.root().join("legacy")).unwrap(), before);
        fs::remove_dir_all(parent).unwrap();

        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let workspace = home.root().join("workspaces/workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("local.yaml"), b"legacy").unwrap();
        assert_eq!(
            inspect_generation(&home).unwrap_err().details["classification"],
            "mixed_generation"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn post_cut_parser_rejects_workspace_before_other_shape_errors() {
        let arguments = ["--workspace", "/tmp/project"].map(OsString::from);
        let error = validate_post_cut_arguments(ProfileOperation::List, &arguments).unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENT");
        assert_eq!(error.details["argument"], "--workspace");
        assert!(validate_post_cut_arguments(ProfileOperation::List, &[]).is_ok());
    }

    #[test]
    fn global_registry_uses_existing_strict_profile_validation() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = GlobalProfileStore::new(&home);
        let invalid = RuntimeProfile {
            argv: vec!["relative".to_owned()],
            codex_home: "/tmp/codex-home".to_owned(),
            environment: BTreeMap::new(),
            native_subagents: crate::workspace::NativeSubagents::Enabled,
        };
        assert_eq!(
            store.add("default".to_owned(), invalid).unwrap_err().code,
            "PROFILE_CONFIG_INVALID"
        );
        assert!(store.load().unwrap().profiles.is_empty());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn registry_crud_preserves_alias_names_and_secure_mode() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = GlobalProfileStore::new(&home);
        let profile = valid_profile(&parent);
        store.add("primary".to_owned(), profile.clone()).unwrap();
        let registry = store.add("alias".to_owned(), profile).unwrap();
        assert_eq!(registry.profiles.len(), 2);
        assert_eq!(registry.profiles["primary"], registry.profiles["alias"]);
        assert_eq!(
            fs::metadata(home.root().join("profiles.yaml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(store.remove("primary").unwrap().profiles.len(), 1);
        assert_eq!(
            store.remove("missing").unwrap_err().code,
            "PROFILE_NOT_FOUND"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn root_lock_serializes_concurrent_registry_updates() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = Arc::new(GlobalProfileStore::new(&home));
        let profile = valid_profile(&parent);
        let threads = (0..8)
            .map(|index| {
                let store = Arc::clone(&store);
                let profile = profile.clone();
                thread::spawn(move || store.add(format!("profile-{index}"), profile).unwrap())
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(store.load().unwrap().profiles.len(), 8);
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn malformed_duplicate_and_insecure_registry_files_fail_closed() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let path = home.root().join("profiles.yaml");
        fs::write(&path, b"schema_version: 1\nprofiles: {}\nprofiles: {}\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            GlobalProfileStore::new(&home).load().unwrap_err().code,
            "PROFILE_CONFIG_INVALID"
        );
        fs::write(&path, b"schema_version: 1\nprofiles: {}\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            GlobalProfileStore::new(&home).load().unwrap_err().code,
            "RUNTIME_PATH_INVALID"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn malformed_and_insecure_markers_fail_closed() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let marker = home.root().join("state.json");
        fs::write(&marker, b"{\"schema_version\":1,\"schema_version\":1}").unwrap();
        assert_eq!(
            inspect_generation(&home).unwrap_err().details["classification"],
            "malformed_marker"
        );
        fs::write(&marker, HOME_STATE_BYTES).unwrap();
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            inspect_generation(&home).unwrap_err().code,
            "RUNTIME_PATH_INVALID"
        );
        fs::remove_dir_all(parent).unwrap();
    }
}
