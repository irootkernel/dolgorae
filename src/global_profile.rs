//! Global Codex Profile registry and hard-cut home generation contract.

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
    b"{\n  \"schema_version\": 2,\n  \"state_generation\": \"global-profile-v2\"\n}\n";
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_BINDING_HISTORY_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeState {
    pub schema_version: u32,
    pub state_generation: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeGenerationStatus {
    Uninitialized,
    GlobalProfileV2,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalProfileRegistry {
    pub schema_version: u32,
    pub profiles: BTreeMap<String, RuntimeProfile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBindingRecord {
    pub selected_name: String,
    pub definition_sha256: String,
    pub server_key: String,
    pub launch_snapshot_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileBindingHistory {
    schema_version: u32,
    bindings: BTreeMap<String, ProfileBindingRecord>,
}

pub struct GlobalProfileStore {
    root: PathBuf,
}

impl GlobalProfileStore {
    #[must_use]
    pub fn new(home: &DolgoraeHome) -> Self {
        Self::from_root(home.root())
    }

    pub(crate) fn from_root(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn load(&self) -> Result<GlobalProfileRegistry, MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        load_registry(&self.root)
    }

    /// Resolve one immutable definition while holding the registry lock.
    ///
    /// Callers retain the returned value through launch preparation. They must
    /// not reopen the registry during admission or recovery.
    pub fn resolve(&self, name: &str) -> Result<RuntimeProfile, MachineError> {
        self.with_resolved(name, Ok)
    }

    /// Execute admission preparation while retaining the root registry lock.
    /// This is the lock-order entry point for registry -> server membership.
    pub fn with_resolved<T>(
        &self,
        name: &str,
        action: impl FnOnce(RuntimeProfile) -> Result<T, MachineError>,
    ) -> Result<T, MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        let profile = load_registry(&self.root)?
            .profiles
            .get(name)
            .cloned()
            .ok_or_else(|| {
                MachineError::new(
                    "PROFILE_NOT_FOUND",
                    "profile was not found",
                    false,
                    json!({"profile": name}),
                )
            })?;
        action(profile)
    }

    /// Remove only after a caller-provided guard has proved that the resolved
    /// definition can be removed. The root lock remains held through commit.
    pub fn remove_if<G>(
        &self,
        name: &str,
        guard: impl FnOnce(&RuntimeProfile) -> Result<G, MachineError>,
    ) -> Result<GlobalProfileRegistry, MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        let mut registry = load_registry_for_removal(&self.root)?;
        let profile = registry.profiles.get(name).ok_or_else(|| {
            MachineError::new(
                "PROFILE_NOT_FOUND",
                "profile was not found",
                false,
                json!({"profile": name}),
            )
        })?;
        let _guard = guard(profile)?;
        registry.profiles.remove(name);
        let mut history = load_binding_history(&self.root)?;
        let original_binding_count = history.bindings.len();
        history
            .bindings
            .retain(|_, record| record.selected_name != name);
        store_registry(&self.root, &registry)?;
        if history.bindings.len() != original_binding_count {
            store_binding_history(&self.root, &history)?;
        }
        Ok(registry)
    }

    pub fn add(
        &self,
        name: String,
        profile: RuntimeProfile,
    ) -> Result<GlobalProfileRegistry, MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        let mut registry = load_registry_or_empty(&self.root)?;
        validate_global_profile_name(&self.root.join("profiles.yaml"), &name)?;
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

    /// Persist the successful name-to-launch resolution after expensive
    /// preparation, revalidating the hand-editable registry under its lock.
    pub fn record_binding(&self, record: ProfileBindingRecord) -> Result<(), MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        let registry = load_registry(&self.root)?;
        let Some(definition) = registry.profiles.get(&record.selected_name) else {
            return Err(MachineError::new(
                "PROFILE_NOT_FOUND",
                "profile changed while its launch contract was prepared",
                false,
                json!({"profile": record.selected_name}),
            ));
        };
        let encoded = serde_json::to_string(definition).map_err(|error| {
            MachineError::profile_config_invalid(self.root.join("profiles.yaml"), error.to_string())
        })?;
        let parsed = crate::jcs::parse(&encoded).map_err(|error| {
            MachineError::profile_config_invalid(self.root.join("profiles.yaml"), error.to_string())
        })?;
        let current_digest =
            crate::jcs::sha256_hex(&crate::jcs::canonicalize(&parsed).map_err(|error| {
                MachineError::profile_config_invalid(
                    self.root.join("profiles.yaml"),
                    error.to_string(),
                )
            })?);
        if current_digest != record.definition_sha256 {
            return Err(MachineError::new(
                "PROFILE_MISMATCH",
                "profile changed while its launch contract was prepared",
                false,
                json!({
                    "profile": record.selected_name,
                    "field": "definition_sha256",
                    "expected": record.definition_sha256,
                    "actual": current_digest,
                }),
            ));
        }
        let mut history = load_binding_history(&self.root)?;
        let identity = format!("{}:{}", record.selected_name, record.server_key);
        if history.bindings.get(&identity) == Some(&record) {
            return Ok(());
        }
        history.bindings.insert(identity, record);
        store_binding_history(&self.root, &history)
    }
}

pub(crate) fn bound_server_keys_under_root_lock(
    root: &Path,
    selected_name: &str,
) -> Result<Vec<String>, MachineError> {
    let history = load_binding_history(root)?;
    let mut keys = history
        .bindings
        .values()
        .filter(|record| record.selected_name == selected_name)
        .map(|record| record.server_key.clone())
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    Ok(keys)
}

/// Revalidate the durable name-to-server binding while the caller holds the
/// corresponding home/server lifecycle locks. Profile removal holds the
/// registry lock before those lifecycle locks, so this read closes the window
/// between snapshot preparation and server publication without reversing the
/// global lock order.
pub(crate) fn require_recorded_binding_under_lifecycle_locks(
    root: &Path,
    selected_name: &str,
    server_key: &str,
) -> Result<(), MachineError> {
    let history = load_binding_history(root)?;
    let identity = format!("{selected_name}:{server_key}");
    if history.bindings.contains_key(&identity) {
        Ok(())
    } else {
        Err(MachineError::new(
            "PROFILE_NOT_FOUND",
            "profile was removed while its server was starting",
            false,
            json!({"profile": selected_name}),
        ))
    }
}

pub fn validate_post_cut_arguments(
    operation: ProfileOperation,
    arguments: &[OsString],
) -> Result<(), MachineError> {
    if arguments
        .iter()
        .take_while(|argument| argument.as_os_str() != "--")
        .any(|argument| {
            argument
                .to_str()
                .is_some_and(|value| value == "--workspace" || value.starts_with("--workspace="))
        })
    {
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

/// Require the active global-Profile home generation before any stateful
/// production command observes workspace or runtime state.
pub fn require_generation(home: &DolgoraeHome) -> Result<(), MachineError> {
    match inspect_root(home.root())? {
        HomeGenerationStatus::GlobalProfileV2 => Ok(()),
        HomeGenerationStatus::Uninitialized => {
            Err(legacy_state_unsupported(home.root(), "uninitialized"))
        }
    }
}

pub fn initialize_generation(home: &DolgoraeHome) -> Result<bool, MachineError> {
    let root = home.root();
    let parent = root
        .parent()
        .ok_or_else(|| MachineError::runtime_path_invalid(root, "Dolgorae home has no parent"))?;
    let directory = File::open(parent)
        .map_err(|error| MachineError::runtime_path_invalid(parent, error.to_string()))?;
    DarwinSystem
        .lock_exclusive(&directory)
        .map_err(|error| MachineError::runtime_path_invalid(parent, error.to_string()))?;
    let created = match inspect_root(root)? {
        HomeGenerationStatus::GlobalProfileV2 => false,
        HomeGenerationStatus::Uninitialized => {
            let staging = parent.join(".dolgorae.initializing");
            if staging.exists() {
                verify_secure_directory(&staging, DarwinSystem.current_uid())?;
                fs::remove_dir_all(&staging).map_err(|error| {
                    MachineError::runtime_path_invalid(&staging, error.to_string())
                })?;
            }
            create_directory(&staging, 0o700)
                .map_err(|error| MachineError::runtime_path_invalid(&staging, error.to_string()))?;
            store_registry(
                &staging,
                &GlobalProfileRegistry {
                    schema_version: 1,
                    profiles: BTreeMap::new(),
                },
            )?;
            store_binding_history(
                &staging,
                &ProfileBindingHistory {
                    schema_version: 1,
                    bindings: BTreeMap::new(),
                },
            )?;
            atomic_create(
                &SystemWorkspacePlatform,
                &staging.join("state.json"),
                HOME_STATE_BYTES,
                0o600,
            )
            .map_err(|error| MachineError::runtime_path_invalid(&staging, error.to_string()))?;
            sync_directory(&staging)
                .map_err(|error| MachineError::runtime_path_invalid(&staging, error.to_string()))?;
            if root.exists() {
                fs::remove_dir(root)
                    .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
            }
            fs::rename(&staging, root)
                .and_then(|()| sync_directory(parent))
                .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
            if inspect_root(root)? != HomeGenerationStatus::GlobalProfileV2 {
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
    if state.schema_version != 2 || state.state_generation != "global-profile-v2" {
        return Err(legacy_state_unsupported(root, "unsupported_generation"));
    }
    if !root.join("profiles.yaml").exists() || !root.join("profile-bindings.json").exists() {
        return Err(legacy_state_unsupported(root, "partial_generation"));
    }
    if contains_legacy_registry(root)? {
        return Err(legacy_state_unsupported(root, "mixed_generation"));
    }
    load_binding_history(root)?;
    Ok(HomeGenerationStatus::GlobalProfileV2)
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
    if inspect_root(root)? == HomeGenerationStatus::Uninitialized {
        return Err(legacy_state_unsupported(root, "uninitialized"));
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
    parse_registry_bytes(&path, &bytes)
}

fn load_registry_for_removal(root: &Path) -> Result<GlobalProfileRegistry, MachineError> {
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
    // Removal must remain possible when the target executable is unavailable.
    // Decode strictly here; store_registry validates the remaining definitions.
    let registry = decode_registry(&path, &bytes)?;
    if registry.schema_version != 1 {
        return Err(MachineError::profile_config_invalid(
            &path,
            "unsupported profile registry schema",
        ));
    }
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

fn load_binding_history(root: &Path) -> Result<ProfileBindingHistory, MachineError> {
    let path = root.join("profile-bindings.json");
    verify_secure_file(&path, DarwinSystem.current_uid())?;
    let bytes = fs::read(&path)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    if bytes.len() as u64 > MAX_BINDING_HISTORY_BYTES {
        return Err(legacy_state_unsupported(root, "malformed_marker"));
    }
    let history: ProfileBindingHistory = serde_json::from_slice(&bytes)
        .map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
    if history.schema_version != 1
        || history.bindings.iter().any(|(identity, record)| {
            identity != &format!("{}:{}", record.selected_name, record.server_key)
                || !is_global_profile_name(&record.selected_name)
                || !is_sha256(&record.definition_sha256)
                || !is_sha256(&record.server_key)
                || !is_sha256(&record.launch_snapshot_sha256)
        })
    {
        return Err(legacy_state_unsupported(root, "malformed_marker"));
    }
    Ok(history)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn store_binding_history(root: &Path, history: &ProfileBindingHistory) -> Result<(), MachineError> {
    let path = root.join("profile-bindings.json");
    let bytes = serde_json::to_vec_pretty(history)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    if bytes.len() as u64 > MAX_BINDING_HISTORY_BYTES {
        return Err(MachineError::runtime_path_invalid(
            &path,
            "Profile binding history exceeds 1 MiB",
        ));
    }
    let temporary = root.join(format!(".profile-bindings-{}.json", Uuid::now_v7()));
    atomic_create(&SystemWorkspacePlatform, &temporary, &bytes, 0o600)
        .map_err(|error| MachineError::runtime_path_invalid(&temporary, error.to_string()))?;
    fs::rename(&temporary, &path)
        .and_then(|()| sync_directory(root))
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))
}

fn parse_registry_bytes(path: &Path, bytes: &[u8]) -> Result<GlobalProfileRegistry, MachineError> {
    let registry = decode_registry(path, bytes)?;
    validate_runtime_profiles(path, registry.schema_version, &registry.profiles)?;
    for name in registry.profiles.keys() {
        validate_global_profile_name(path, name)?;
    }
    Ok(registry)
}

fn decode_registry(path: &Path, bytes: &[u8]) -> Result<GlobalProfileRegistry, MachineError> {
    // Value rejects duplicate mapping keys at every depth; direct BTreeMap
    // deserialization would silently keep the last value instead.
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(bytes)
        .map_err(|error| MachineError::profile_config_invalid(path, error.to_string()))?;
    serde_yaml_ng::from_value(value)
        .map_err(|error| MachineError::profile_config_invalid(path, error.to_string()))
}

fn validate_global_profile_name(path: &Path, name: &str) -> Result<(), MachineError> {
    if is_global_profile_name(name) {
        return Ok(());
    }
    Err(MachineError::profile_config_invalid(
        path,
        "profile names must match ^[a-z0-9][a-z0-9._-]{0,127}$",
    ))
}

fn is_global_profile_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && name.len() <= 128
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
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
            HomeGenerationStatus::GlobalProfileV2
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
    fn initialization_recovers_a_private_partial_staging_directory() {
        let (parent, home) = test_home();
        let staging = parent.join(".dolgorae.initializing");
        fs::create_dir(&staging).unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(staging.join("partial"), b"interrupted").unwrap();
        assert!(initialize_generation(&home).unwrap());
        assert!(!staging.exists());
        assert_eq!(
            inspect_generation(&home).unwrap(),
            HomeGenerationStatus::GlobalProfileV2
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn initialization_preserves_an_insecure_staging_directory() {
        let (parent, home) = test_home();
        let staging = parent.join(".dolgorae.initializing");
        fs::create_dir(&staging).unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o777)).unwrap();
        fs::write(staging.join("partial"), b"preserve").unwrap();
        assert_eq!(
            initialize_generation(&home).unwrap_err().code,
            "RUNTIME_PATH_INVALID"
        );
        assert_eq!(fs::read(staging.join("partial")).unwrap(), b"preserve");
        assert_eq!(
            fs::metadata(&staging).unwrap().permissions().mode() & 0o777,
            0o777
        );
        assert!(!home.root().exists());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn an_empty_home_that_changes_after_the_cli_gate_keeps_its_classification() {
        let (parent, home) = test_home();
        fs::create_dir(home.root()).unwrap();
        fs::set_permissions(home.root(), fs::Permissions::from_mode(0o700)).unwrap();
        let error = GlobalProfileStore::new(&home).load().unwrap_err();
        assert_eq!(error.code, "LEGACY_STATE_UNSUPPORTED");
        assert_eq!(error.details["classification"], "uninitialized");
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
        let trailing = ["default", "--", "--workspace"].map(OsString::from);
        assert!(validate_post_cut_arguments(ProfileOperation::Add, &trailing).is_ok());
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
    fn global_registry_names_match_the_persisted_binding_contract() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = GlobalProfileStore::new(&home);
        let profile = valid_profile(&parent);
        for invalid in [
            "",
            "Default",
            "with space",
            "with:colon",
            "-leading",
            &"a".repeat(129),
            "é",
        ] {
            let error = store.add(invalid.to_owned(), profile.clone()).unwrap_err();
            assert_eq!(error.code, "PROFILE_CONFIG_INVALID", "for {invalid:?}");
            assert!(store.load().unwrap().profiles.is_empty());
        }
        let boundary = format!("a{}", "z".repeat(127));
        assert!(store.add(boundary.clone(), profile).is_ok());
        assert!(store.load().unwrap().profiles.contains_key(&boundary));
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn hand_edited_global_registry_rejects_a_nonconformant_name_without_mutation() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let path = home.root().join("profiles.yaml");
        let registry = GlobalProfileRegistry {
            schema_version: 1,
            profiles: BTreeMap::from([("Default".to_owned(), valid_profile(&parent))]),
        };
        let bytes = serde_yaml_ng::to_string(&registry).unwrap().into_bytes();
        fs::write(&path, &bytes).unwrap();
        let error = GlobalProfileStore::new(&home).load().unwrap_err();
        assert_eq!(error.code, "PROFILE_CONFIG_INVALID");
        assert_eq!(fs::read(&path).unwrap(), bytes);
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
        assert_eq!(
            store
                .remove_if("primary", |_| Ok(()))
                .unwrap()
                .profiles
                .len(),
            1
        );
        assert_eq!(
            store.remove_if("missing", |_| Ok(())).unwrap_err().code,
            "PROFILE_NOT_FOUND"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn binding_history_skips_identical_writes_and_prunes_removed_names() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = GlobalProfileStore::new(&home);
        let profile = valid_profile(&parent);
        store.add("primary".to_owned(), profile.clone()).unwrap();
        let encoded = serde_json::to_string(&profile).unwrap();
        let parsed = crate::jcs::parse(&encoded).unwrap();
        let definition_sha256 = crate::jcs::sha256_hex(&crate::jcs::canonicalize(&parsed).unwrap());
        let stale_digest = "c".repeat(64);
        let mismatch = store
            .record_binding(ProfileBindingRecord {
                selected_name: "primary".to_owned(),
                definition_sha256: stale_digest.clone(),
                server_key: "a".repeat(64),
                launch_snapshot_sha256: "b".repeat(64),
            })
            .unwrap_err();
        assert_eq!(mismatch.code, "PROFILE_MISMATCH");
        assert_eq!(
            mismatch.details,
            json!({
                "profile": "primary",
                "field": "definition_sha256",
                "expected": stale_digest,
                "actual": definition_sha256,
            })
        );
        let record = ProfileBindingRecord {
            selected_name: "primary".to_owned(),
            definition_sha256: definition_sha256.clone(),
            server_key: "a".repeat(64),
            launch_snapshot_sha256: "b".repeat(64),
        };
        store.record_binding(record.clone()).unwrap();
        require_recorded_binding_under_lifecycle_locks(home.root(), "primary", &"a".repeat(64))
            .unwrap();
        let path = home.root().join("profile-bindings.json");
        let unchanged = fs::read(&path).unwrap();
        store.record_binding(record.clone()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), unchanged);

        store.remove_if("primary", |_| Ok(())).unwrap();
        let registry_after_removal = fs::read(home.root().join("profiles.yaml")).unwrap();
        let history_after_removal = fs::read(&path).unwrap();
        assert_eq!(
            store.record_binding(record).unwrap_err().code,
            "PROFILE_NOT_FOUND"
        );
        assert_eq!(
            fs::read(home.root().join("profiles.yaml")).unwrap(),
            registry_after_removal
        );
        assert_eq!(fs::read(&path).unwrap(), history_after_removal);
        assert_eq!(
            require_recorded_binding_under_lifecycle_locks(
                home.root(),
                "primary",
                &"a".repeat(64),
            )
            .unwrap_err()
            .code,
            "PROFILE_NOT_FOUND"
        );
        assert!(
            load_binding_history(home.root())
                .unwrap()
                .bindings
                .is_empty()
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn removal_retains_the_caller_guard_through_both_registry_commits() {
        struct CommitGuard {
            root: PathBuf,
            selected_name: String,
        }
        impl Drop for CommitGuard {
            fn drop(&mut self) {
                let registry = load_registry(&self.root).unwrap();
                assert!(!registry.profiles.contains_key(&self.selected_name));
                let history = load_binding_history(&self.root).unwrap();
                assert!(
                    history
                        .bindings
                        .values()
                        .all(|record| record.selected_name != self.selected_name)
                );
            }
        }

        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = GlobalProfileStore::new(&home);
        let profile = valid_profile(&parent);
        store.add("primary".to_owned(), profile.clone()).unwrap();
        let encoded = serde_json::to_string(&profile).unwrap();
        let parsed = crate::jcs::parse(&encoded).unwrap();
        store
            .record_binding(ProfileBindingRecord {
                selected_name: "primary".to_owned(),
                definition_sha256: crate::jcs::sha256_hex(
                    &crate::jcs::canonicalize(&parsed).unwrap(),
                ),
                server_key: "a".repeat(64),
                launch_snapshot_sha256: "b".repeat(64),
            })
            .unwrap();
        store
            .remove_if("primary", |_| {
                Ok(CommitGuard {
                    root: home.root().to_path_buf(),
                    selected_name: "primary".to_owned(),
                })
            })
            .unwrap();
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
    fn nested_duplicate_keys_fail_before_registry_reads_or_removal() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let profile = serde_json::to_string(&valid_profile(&parent)).unwrap();
        let registry = format!(r#"{{"schema_version":1,"profiles":{{"primary":{profile}}}}}"#);
        let cases = [
            format!(
                r#"{{"schema_version":1,"profiles":{{"primary":{profile},"primary":{profile}}}}}"#
            ),
            registry.replace(
                r#""LANG":"C.UTF-8""#,
                r#""LANG":"invalid","LANG":"C.UTF-8""#,
            ),
            "schema_version: 1\nprofiles: {}\nprofiles: {}\n".to_owned(),
        ];
        let path = home.root().join("profiles.yaml");
        let history = fs::read(home.root().join("profile-bindings.json")).unwrap();
        let store = GlobalProfileStore::new(&home);
        for contents in cases {
            assert_ne!(contents, registry);
            fs::write(&path, &contents).unwrap();
            assert_eq!(store.load().unwrap_err().code, "PROFILE_CONFIG_INVALID");
            assert_eq!(
                store
                    .remove_if::<()>("primary", |_| panic!("ambiguous registry reached guard"))
                    .unwrap_err()
                    .code,
                "PROFILE_CONFIG_INVALID"
            );
            assert_eq!(fs::read(&path).unwrap(), contents.as_bytes());
            assert_eq!(
                fs::read(home.root().join("profile-bindings.json")).unwrap(),
                history
            );
        }
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn secure_home_components_reject_a_different_expected_owner() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let uid = DarwinSystem.current_uid();
        let foreign_uid = uid.wrapping_add(1);
        verify_secure_directory(home.root(), uid).unwrap();
        assert_eq!(
            verify_secure_directory(home.root(), foreign_uid)
                .unwrap_err()
                .code,
            "RUNTIME_PATH_INVALID"
        );
        for name in ["state.json", "profiles.yaml", "profile-bindings.json"] {
            let path = home.root().join(name);
            let before = fs::read(&path).unwrap();
            verify_secure_file(&path, uid).unwrap();
            assert_eq!(
                verify_secure_file(&path, foreign_uid).unwrap_err().code,
                "RUNTIME_PATH_INVALID"
            );
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn interrupted_removal_retains_history_until_a_later_guarded_removal() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = GlobalProfileStore::new(&home);
        let profile = valid_profile(&parent);
        store.add("primary".to_owned(), profile.clone()).unwrap();
        let definition_sha256 = crate::jcs::sha256_hex(
            &crate::jcs::canonicalize(
                &crate::jcs::parse(&serde_json::to_string(&profile).unwrap()).unwrap(),
            )
            .unwrap(),
        );
        store
            .record_binding(ProfileBindingRecord {
                selected_name: "primary".to_owned(),
                definition_sha256,
                server_key: "a".repeat(64),
                launch_snapshot_sha256: "b".repeat(64),
            })
            .unwrap();
        // Model the durable boundary after registry replacement, before history pruning.
        store_registry(
            home.root(),
            &GlobalProfileRegistry {
                schema_version: 1,
                profiles: BTreeMap::new(),
            },
        )
        .unwrap();
        assert_eq!(
            store.resolve("primary").unwrap_err().code,
            "PROFILE_NOT_FOUND"
        );
        assert_eq!(
            bound_server_keys_under_root_lock(home.root(), "primary").unwrap(),
            vec!["a".repeat(64)]
        );
        store.add("primary".to_owned(), profile).unwrap();
        let history = fs::read(home.root().join("profile-bindings.json")).unwrap();
        assert_eq!(
            store
                .remove_if("primary", |_| Err::<(), _>(
                    MachineError::profile_config_invalid(home.root(), "guard denied")
                ))
                .unwrap_err()
                .code,
            "PROFILE_CONFIG_INVALID"
        );
        assert_eq!(
            fs::read(home.root().join("profile-bindings.json")).unwrap(),
            history
        );
        store.remove_if("primary", |_| Ok(())).unwrap();
        assert!(
            bound_server_keys_under_root_lock(home.root(), "primary")
                .unwrap()
                .is_empty()
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

    #[test]
    fn a_marker_without_its_global_registry_is_partial_generation_state() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        fs::remove_file(home.root().join("profiles.yaml")).unwrap();
        let error = inspect_generation(&home).unwrap_err();
        assert_eq!(error.code, "LEGACY_STATE_UNSUPPORTED");
        assert_eq!(error.details["classification"], "partial_generation");
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn legacy_and_binding_history_generation_failures_are_exact_and_non_mutating() {
        type GenerationMutation = fn(&Path);
        let cases: &[(&str, GenerationMutation)] = &[
            ("unsupported_generation", |root| {
                fs::write(
                    root.join("state.json"),
                    b"{\"schema_version\":1,\"state_generation\":\"global-profile-v1\"}\n",
                )
                .unwrap();
            }),
            ("partial_generation", |root| {
                fs::remove_file(root.join("profile-bindings.json")).unwrap();
            }),
            ("malformed_marker", |root| {
                fs::write(root.join("profile-bindings.json"), b"{not-json}\n").unwrap();
            }),
            ("malformed_marker", |root| {
                let digest = "a".repeat(64);
                let identity = format!("Default:{digest}");
                let bytes = serde_json::to_vec_pretty(&json!({
                    "schema_version": 1,
                    "bindings": {
                        (identity): {
                            "selected_name": "Default",
                            "definition_sha256": digest.clone(),
                            "server_key": "b".repeat(64),
                            "launch_snapshot_sha256": "c".repeat(64),
                        }
                    }
                }))
                .unwrap();
                fs::write(root.join("profile-bindings.json"), bytes).unwrap();
            }),
            ("malformed_marker", |root| {
                fs::write(
                    root.join("profile-bindings.json"),
                    vec![b'x'; usize::try_from(MAX_BINDING_HISTORY_BYTES).unwrap() + 1],
                )
                .unwrap();
            }),
        ];
        for (classification, mutate) in cases {
            let (parent, home) = test_home();
            initialize_generation(&home).unwrap();
            mutate(home.root());
            let marker_before = fs::read(home.root().join("state.json")).unwrap();
            let registry_before = fs::read(home.root().join("profiles.yaml")).unwrap();
            let history_before = fs::read(home.root().join("profile-bindings.json")).ok();
            let error = inspect_generation(&home).unwrap_err();
            assert_eq!(error.code, "LEGACY_STATE_UNSUPPORTED");
            assert_eq!(error.details["classification"], *classification);
            assert_eq!(
                fs::read(home.root().join("state.json")).unwrap(),
                marker_before
            );
            assert_eq!(
                fs::read(home.root().join("profiles.yaml")).unwrap(),
                registry_before
            );
            assert_eq!(
                fs::read(home.root().join("profile-bindings.json")).ok(),
                history_before
            );
            fs::remove_dir_all(parent).unwrap();
        }
    }
}
