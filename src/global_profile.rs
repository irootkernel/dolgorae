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
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const HOME_STATE_BYTES: &[u8] =
    b"{\n  \"schema_version\": 2,\n  \"state_generation\": \"global-profile-v2\"\n}\n";
const MAX_HOME_STATE_BYTES: u64 = 4096;
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
    #[serde(deserialize_with = "binding_name")]
    pub selected_name: String,
    #[serde(deserialize_with = "binding_digest")]
    pub definition_sha256: String,
    #[serde(deserialize_with = "binding_digest")]
    pub server_key: String,
    #[serde(deserialize_with = "binding_digest")]
    pub launch_snapshot_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileBindingHistory {
    #[serde(deserialize_with = "binding_version")]
    schema_version: u32,
    #[serde(deserialize_with = "binding_map")]
    bindings: BTreeMap<String, ProfileBindingRecord>,
}

fn binding_version<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(de)?;
    if version == 1 {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(
            "unsupported binding history version",
        ))
    }
}
fn binding_name<'de, D: serde::Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let name = String::deserialize(de)?;
    if is_global_profile_name(&name) {
        Ok(name)
    } else {
        Err(serde::de::Error::custom("invalid binding name"))
    }
}
fn binding_digest<'de, D: serde::Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let value = String::deserialize(de)?;
    if is_sha256(&value) {
        Ok(value)
    } else {
        Err(serde::de::Error::custom("invalid binding digest"))
    }
}
fn binding_map<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<String, ProfileBindingRecord>, D::Error> {
    struct Bindings;
    impl<'de> serde::de::Visitor<'de> for Bindings {
        type Value = BTreeMap<String, ProfileBindingRecord>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("checked binding identities")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut bindings = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                if bindings.contains_key(&key) {
                    return Err(serde::de::Error::custom("duplicate binding identity"));
                }
                if !key
                    .split_once(':')
                    .is_some_and(|(name, digest)| is_global_profile_name(name) && is_sha256(digest))
                {
                    return Err(serde::de::Error::custom("invalid binding identity"));
                }
                let record: ProfileBindingRecord = map.next_value()?;
                if key != format!("{}:{}", record.selected_name, record.server_key) {
                    return Err(serde::de::Error::custom("binding identity mismatch"));
                }
                bindings.insert(key, record);
            }
            Ok(bindings)
        }
    }
    de.deserialize_map(Bindings)
}

pub struct GlobalProfileStore {
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingMaintenanceBarrier {
    SourceSynced,
    SourceDirectorySynced,
    TargetSynced,
    SelectionReplaced,
    SelectionSynced,
}

impl GlobalProfileStore {
    pub(crate) fn with_maintenance_lock<T>(
        &self,
        action: impl FnOnce(&Path) -> Result<T, MachineError>,
    ) -> Result<T, MachineError> {
        let _lock = lock_initialized_home(&self.root)?;
        action(&self.root)
    }
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
        if self.root.join("profile-maintenance-active.json").exists()
            && let Some(record) = history
                .bindings
                .values()
                .find(|record| record.selected_name == name)
        {
            return Err(binding_maintenance_busy(name, &record.server_key));
        }
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
        if self.root.join("profile-maintenance-active.json").exists() {
            return Err(binding_maintenance_busy(
                &record.selected_name,
                &record.server_key,
            ));
        }
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

/// Exact qualified selection is checked again under the existing lifecycle
/// locks. This reads the current registry and history without recording either.
pub(crate) fn require_exact_binding_under_lifecycle_locks(
    root: &Path,
    expected: &ProfileBindingRecord,
) -> Result<(), MachineError> {
    let registry = load_registry(root)?;
    let definition = registry
        .profiles
        .get(&expected.selected_name)
        .ok_or_else(|| {
            MachineError::new(
                "PROFILE_NOT_FOUND",
                "selected Profile is absent",
                false,
                json!({"profile":expected.selected_name}),
            )
        })?;
    let text = serde_json::to_string(definition).map_err(|e| {
        MachineError::profile_config_invalid(root.join("profiles.yaml"), e.to_string())
    })?;
    let parsed = crate::jcs::parse(&text).map_err(|e| {
        MachineError::profile_config_invalid(root.join("profiles.yaml"), e.to_string())
    })?;
    let current = crate::jcs::sha256_hex(&crate::jcs::canonicalize(&parsed).map_err(|e| {
        MachineError::profile_config_invalid(root.join("profiles.yaml"), e.to_string())
    })?);
    let history = load_binding_history(root)?;
    if current != expected.definition_sha256
        || history.bindings.get(&format!(
            "{}:{}",
            expected.selected_name, expected.server_key
        )) != Some(expected)
    {
        return Err(MachineError::new(
            "PROFILE_MISMATCH",
            "selected Profile binding changed before retirement",
            false,
            json!({"profile":expected.selected_name,"field":"recorded_binding","expected":expected,"actual":history.bindings.get(&format!("{}:{}",expected.selected_name,expected.server_key))}),
        ));
    }
    Ok(())
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
    let bytes = read_prefix(&marker, MAX_HOME_STATE_BYTES)
        .map_err(|error| MachineError::runtime_path_invalid(&marker, error.to_string()))?;
    if bytes.len() as u64 > MAX_HOME_STATE_BYTES {
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

fn read_prefix(path: &Path, maximum: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn load_registry(root: &Path) -> Result<GlobalProfileRegistry, MachineError> {
    let path = root.join("profiles.yaml");
    verify_secure_file(&path, DarwinSystem.current_uid())?;
    let bytes = read_prefix(&path, MAX_REGISTRY_BYTES)
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
    let bytes = read_prefix(&path, MAX_REGISTRY_BYTES)
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
    let bytes = read_prefix(&path, MAX_BINDING_HISTORY_BYTES)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    parse_binding_history(root, &bytes)
}

fn parse_binding_history(root: &Path, bytes: &[u8]) -> Result<ProfileBindingHistory, MachineError> {
    if bytes.len() as u64 > MAX_BINDING_HISTORY_BYTES {
        let text = match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?
            }
            Err(_) => return Err(legacy_state_unsupported(root, "malformed_marker")),
        };
        if text.trim_start().starts_with('{') {
            match crate::jcs::parse(text) {
                Ok(_) => {
                    // A complete bounded prefix must also have the checked shape.
                    let history: ProfileBindingHistory = serde_json::from_str(text)
                        .map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
                    validate_binding_history(root, &history)?;
                }
                Err(crate::jcs::JcsError::Json(error)) if error.is_eof() => {
                    match serde_json::from_str::<ProfileBindingHistory>(text) {
                        Err(error) if error.is_eof() => {}
                        Ok(_) => {}
                        Err(_) => return Err(legacy_state_unsupported(root, "malformed_marker")),
                    }
                }
                Err(_) => return Err(legacy_state_unsupported(root, "malformed_marker")),
            }
            return Err(binding_capacity());
        }
        return Err(legacy_state_unsupported(root, "malformed_marker"));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
    crate::jcs::parse(text).map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
    let history: ProfileBindingHistory = serde_json::from_slice(bytes)
        .map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
    validate_binding_history(root, &history)?;
    Ok(history)
}

fn validate_binding_history(
    root: &Path,
    history: &ProfileBindingHistory,
) -> Result<(), MachineError> {
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
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn store_binding_history(root: &Path, history: &ProfileBindingHistory) -> Result<(), MachineError> {
    let path = root.join("profile-bindings.json");
    let bytes = serde_json::to_vec(history)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    if bytes.len() as u64 > MAX_BINDING_HISTORY_BYTES {
        return Err(binding_capacity());
    }
    let temporary = root.join(format!(".profile-bindings-{}.json", Uuid::now_v7()));
    atomic_create(&SystemWorkspacePlatform, &temporary, &bytes, 0o600)
        .map_err(|error| MachineError::runtime_path_invalid(&temporary, error.to_string()))?;
    fs::rename(&temporary, &path)
        .and_then(|()| sync_directory(root))
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))
}

fn binding_capacity() -> MachineError {
    MachineError::new(
        "STORAGE_CAPACITY_EXCEEDED",
        "Profile binding history exceeds 1 MiB",
        false,
        json!({"required_action":"observe_original_maintenance"}),
    )
}

fn binding_maintenance_busy(profile: &str, server_key: &str) -> MachineError {
    MachineError::new(
        "PROFILE_SERVER_BUSY",
        "retained Profile maintenance closes binding admission",
        true,
        json!({"profile":profile,"server_key":server_key,"reason":"resume the original Profile maintenance before changing binding history"}),
    )
}

pub(crate) fn binding_maintenance_basis(root: &Path) -> Result<(String, Vec<u8>), MachineError> {
    let history = load_binding_history(root)?;
    let path = root.join("profile-bindings.json");
    let source = read_prefix(&path, MAX_BINDING_HISTORY_BYTES)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    let target = serde_json::to_vec(&history)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    if target.len() as u64 > MAX_BINDING_HISTORY_BYTES {
        return Err(binding_capacity());
    }
    Ok((crate::jcs::sha256_hex(&source), target))
}

pub(crate) fn verify_binding_maintenance_completion(
    root: &Path,
    operation_id: Uuid,
    expected_digest: &str,
    verify_selected: bool,
) -> Result<(), MachineError> {
    let generations = root.join("binding-generations");
    let directory = generations.join(operation_id.to_string());
    let source_path = directory.join("source.json");
    verify_secure_directory(&generations, DarwinSystem.current_uid())?;
    verify_secure_directory(&directory, DarwinSystem.current_uid())?;
    verify_secure_file(&source_path, DarwinSystem.current_uid())?;
    let source = read_prefix(&source_path, MAX_BINDING_HISTORY_BYTES)
        .map_err(|error| MachineError::runtime_path_invalid(&source_path, error.to_string()))?;
    if source.len() as u64 > MAX_BINDING_HISTORY_BYTES {
        return Err(MachineError::new(
            "LOOKUP_NAMESPACE_CORRUPT",
            "retained binding source exceeds its approved bound",
            false,
            json!({}),
        ));
    }
    let original = parse_binding_history(root, &source)?;
    if crate::jcs::sha256_hex(&source) != expected_digest {
        return Err(MachineError::new(
            "LOOKUP_NAMESPACE_CORRUPT",
            "retained binding source changed",
            false,
            json!({}),
        ));
    }
    if verify_selected {
        let path = root.join("profile-bindings.json");
        verify_secure_file(&path, DarwinSystem.current_uid())?;
        let selected = read_prefix(&path, MAX_BINDING_HISTORY_BYTES)
            .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
        let target = serde_json::to_vec(&original)
            .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
        if selected != target {
            return Err(MachineError::new(
                "LOOKUP_NAMESPACE_CORRUPT",
                "selected binding history contradicts retained completion",
                false,
                json!({}),
            ));
        }
    }
    Ok(())
}

pub(crate) fn compact_binding_history(
    root: &Path,
    operation_id: Uuid,
    expected_digest: &str,
    barrier: &mut impl FnMut(BindingMaintenanceBarrier) -> Result<(), MachineError>,
) -> Result<(), MachineError> {
    let path = root.join("profile-bindings.json");
    let (current_digest, target) = binding_maintenance_basis(root)?;
    let generations = root.join("binding-generations");
    let directory = generations.join(operation_id.to_string());
    let source_path = directory.join("source.json");
    if current_digest != expected_digest {
        verify_secure_directory(&generations, DarwinSystem.current_uid())?;
        verify_secure_directory(&directory, DarwinSystem.current_uid())?;
        verify_secure_file(&source_path, DarwinSystem.current_uid())?;
        let source = read_prefix(&source_path, MAX_BINDING_HISTORY_BYTES)
            .map_err(|error| MachineError::runtime_path_invalid(&source_path, error.to_string()))?;
        let original: ProfileBindingHistory = serde_json::from_slice(&source)
            .map_err(|_| legacy_state_unsupported(root, "malformed_marker"))?;
        let applied = read_prefix(&path, MAX_BINDING_HISTORY_BYTES)
            .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
        if crate::jcs::sha256_hex(&source) == expected_digest
            && applied
                == serde_json::to_vec(&original)
                    .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?
        {
            sync_directory(root)
                .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
            return Ok(());
        }
        return Err(MachineError::new(
            "MAINTENANCE_PLAN_STALE",
            "Profile binding history changed since planning",
            false,
            json!({"operation_ref":operation_id}),
        ));
    }
    if operation_id.get_version_num() != 7 {
        return Err(MachineError::invalid_argument(
            "plan_id",
            "original UUIDv7 is required",
        ));
    }
    if !generations.exists() {
        create_directory(&generations, 0o700)
            .map_err(|error| MachineError::runtime_path_invalid(&generations, error.to_string()))?;
    }
    verify_secure_directory(&generations, DarwinSystem.current_uid())?;
    if !directory.exists() {
        create_directory(&directory, 0o700)
            .map_err(|error| MachineError::runtime_path_invalid(&directory, error.to_string()))?;
    }
    verify_secure_directory(&directory, DarwinSystem.current_uid())?;
    let source = read_prefix(&path, MAX_BINDING_HISTORY_BYTES)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    if source_path.exists() {
        verify_secure_file(&source_path, DarwinSystem.current_uid())?;
        let retained = read_prefix(&source_path, MAX_BINDING_HISTORY_BYTES)
            .map_err(|error| MachineError::runtime_path_invalid(&source_path, error.to_string()))?;
        if retained != source {
            return Err(MachineError::new(
                "LOOKUP_NAMESPACE_CORRUPT",
                "retained Profile binding source changed",
                false,
                json!({"operation_ref":operation_id,"effect":"unresolved","required_action":"restore_state"}),
            ));
        }
    } else {
        atomic_create(&SystemWorkspacePlatform, &source_path, &source, 0o600)
            .map_err(|error| MachineError::runtime_path_invalid(&source_path, error.to_string()))?;
    }
    barrier(BindingMaintenanceBarrier::SourceSynced)?;
    sync_directory(&directory)
        .and_then(|()| sync_directory(&generations))
        .and_then(|()| sync_directory(root))
        .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
    barrier(BindingMaintenanceBarrier::SourceDirectorySynced)?;
    let temporary = directory.join("target.json");
    if temporary.exists() {
        verify_secure_file(&temporary, DarwinSystem.current_uid())?;
        if read_prefix(&temporary, MAX_BINDING_HISTORY_BYTES)
            .map_err(|error| MachineError::runtime_path_invalid(&temporary, error.to_string()))?
            != target
        {
            return Err(MachineError::new(
                "LOOKUP_NAMESPACE_CORRUPT",
                "prepared Profile binding target changed",
                false,
                json!({"operation_ref":operation_id,"effect":"unresolved","required_action":"restore_state"}),
            ));
        }
    } else {
        atomic_create(&SystemWorkspacePlatform, &temporary, &target, 0o600)
            .map_err(|error| MachineError::runtime_path_invalid(&temporary, error.to_string()))?;
    }
    sync_directory(&directory)
        .map_err(|error| MachineError::runtime_path_invalid(&directory, error.to_string()))?;
    barrier(BindingMaintenanceBarrier::TargetSynced)?;
    fs::rename(&temporary, &path)
        .map_err(|error| MachineError::runtime_path_invalid(&path, error.to_string()))?;
    barrier(BindingMaintenanceBarrier::SelectionReplaced)?;
    sync_directory(root)
        .and_then(|()| sync_directory(&directory))
        .map_err(|error| MachineError::runtime_path_invalid(root, error.to_string()))?;
    barrier(BindingMaintenanceBarrier::SelectionSynced)?;
    Ok(())
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
    fn native_config_profile_is_rejected_before_registry_persistence() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = GlobalProfileStore::new(&home);
        let path = home.root().join("profiles.yaml");
        let empty_bytes = fs::read(&path).unwrap();
        let mut rejected = valid_profile(&parent);
        rejected
            .argv
            .extend(["--profile".to_owned(), "selected".to_owned()]);
        let error = store
            .add("selected".to_owned(), rejected.clone())
            .unwrap_err();
        assert_eq!(error.code, "PROFILE_CONFIG_INVALID");
        assert_eq!(fs::read(&path).unwrap(), empty_bytes);
        assert!(store.load().unwrap().profiles.is_empty());

        let mut supported = valid_profile(&parent);
        supported.argv.extend(
            [
                "--enable",
                "shell_tool",
                "--disable",
                "shell_tool",
                "--enable",
                "shell_tool",
                "--strict-config",
            ]
            .map(str::to_owned),
        );
        store.add("default".to_owned(), supported.clone()).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(
            store.load().unwrap().profiles["default"].argv,
            supported.argv
        );
        assert_eq!(
            store.add("selected".to_owned(), rejected).unwrap_err().code,
            "PROFILE_CONFIG_INVALID"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(store.load().unwrap().profiles.len(), 1);
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn hand_edited_native_config_profile_is_rejected_without_mutation() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let path = home.root().join("profiles.yaml");
        let mut profile = valid_profile(&parent);
        profile
            .argv
            .extend(["--profile".to_owned(), "selected".to_owned()]);
        let registry = GlobalProfileRegistry {
            schema_version: 1,
            profiles: BTreeMap::from([("default".to_owned(), profile)]),
        };
        let bytes = serde_yaml_ng::to_string(&registry).unwrap().into_bytes();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            GlobalProfileStore::new(&home).load().unwrap_err().code,
            "PROFILE_CONFIG_INVALID"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
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
    fn compacted_binding_history_keeps_capacity_for_the_next_normal_write() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let store = GlobalProfileStore::new(&home);
        let profile = valid_profile(&parent);
        store.add("primary".into(), profile.clone()).unwrap();
        let definition_sha256 = crate::jcs::sha256_hex(
            &crate::jcs::canonicalize(
                &crate::jcs::parse(&serde_json::to_string(&profile).unwrap()).unwrap(),
            )
            .unwrap(),
        );
        let mut history = load_binding_history(home.root()).unwrap();
        for index in 0..10_000 {
            let record = ProfileBindingRecord {
                selected_name: "primary".into(),
                definition_sha256: definition_sha256.clone(),
                server_key: format!("{index:064x}"),
                launch_snapshot_sha256: "b".repeat(64),
            };
            history
                .bindings
                .insert(format!("primary:{}", record.server_key), record);
            if index % 128 == 0
                && serde_json::to_vec_pretty(&history).unwrap().len() as u64
                    > MAX_BINDING_HISTORY_BYTES
            {
                break;
            }
        }
        while serde_json::to_vec_pretty(&history).unwrap().len() as u64 > MAX_BINDING_HISTORY_BYTES
        {
            history.bindings.pop_last().unwrap();
        }
        let source = serde_json::to_vec_pretty(&history).unwrap();
        assert!(source.len() as u64 <= MAX_BINDING_HISTORY_BYTES);
        let next = ProfileBindingRecord {
            selected_name: "primary".into(),
            definition_sha256,
            server_key: "f".repeat(64),
            launch_snapshot_sha256: "c".repeat(64),
        };
        let mut expected = history.clone();
        expected
            .bindings
            .insert(format!("primary:{}", next.server_key), next.clone());
        assert!(
            serde_json::to_vec_pretty(&expected).unwrap().len() as u64 > MAX_BINDING_HISTORY_BYTES
        );
        let path = home.root().join("profile-bindings.json");
        fs::write(&path, &source).unwrap();
        let operation = Uuid::now_v7();
        store
            .with_maintenance_lock(|root| {
                compact_binding_history(
                    root,
                    operation,
                    &crate::jcs::sha256_hex(&source),
                    &mut |_| Ok(()),
                )
            })
            .unwrap();
        store.record_binding(next).unwrap();
        assert_eq!(load_binding_history(home.root()).unwrap(), expected);
        assert!(fs::metadata(&path).unwrap().len() <= MAX_BINDING_HISTORY_BYTES);
        assert_eq!(
            fs::read(
                home.root()
                    .join("binding-generations")
                    .join(operation.to_string())
                    .join("source.json")
            )
            .unwrap(),
            source
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn binding_compaction_preserves_identity_at_each_crash_boundary() {
        for fault in [
            BindingMaintenanceBarrier::SourceSynced,
            BindingMaintenanceBarrier::SourceDirectorySynced,
            BindingMaintenanceBarrier::TargetSynced,
            BindingMaintenanceBarrier::SelectionReplaced,
            BindingMaintenanceBarrier::SelectionSynced,
        ] {
            let (parent, home) = test_home();
            initialize_generation(&home).unwrap();
            let store = GlobalProfileStore::new(&home);
            let profile = valid_profile(&parent);
            store.add("primary".into(), profile.clone()).unwrap();
            let record = ProfileBindingRecord {
                selected_name: "primary".into(),
                definition_sha256: crate::jcs::sha256_hex(
                    &crate::jcs::canonicalize(
                        &crate::jcs::parse(&serde_json::to_string(&profile).unwrap()).unwrap(),
                    )
                    .unwrap(),
                ),
                server_key: "a".repeat(64),
                launch_snapshot_sha256: "b".repeat(64),
            };
            store.record_binding(record.clone()).unwrap();
            // Existing homes may still contain the old pretty representation.
            let path = home.root().join("profile-bindings.json");
            fs::write(
                &path,
                serde_json::to_vec_pretty(&load_binding_history(home.root()).unwrap()).unwrap(),
            )
            .unwrap();
            let source = fs::read(home.root().join("profile-bindings.json")).unwrap();
            let expected = crate::jcs::sha256_hex(&source);
            let operation = Uuid::now_v7();
            store
                .with_maintenance_lock(|root| {
                    assert!(
                        compact_binding_history(root, operation, &expected, &mut |point| {
                            if point == fault {
                                Err(MachineError::new(
                                    "RECOVERY_REQUIRED",
                                    "injected binding crash",
                                    false,
                                    json!({}),
                                ))
                            } else {
                                Ok(())
                            }
                        })
                        .is_err()
                    );
                    compact_binding_history(root, operation, &expected, &mut |_| Ok(()))?;
                    compact_binding_history(root, operation, &expected, &mut |_| Ok(()))?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                load_binding_history(home.root()).unwrap().bindings["primary:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
                record
            );
            assert_eq!(
                fs::read(
                    home.root()
                        .join("binding-generations")
                        .join(operation.to_string())
                        .join("source.json")
                )
                .unwrap(),
                source
            );
            assert!(
                fs::metadata(home.root().join("profile-bindings.json"))
                    .unwrap()
                    .len()
                    < source.len() as u64
            );
            store
                .with_maintenance_lock(|root| {
                    verify_binding_maintenance_completion(root, operation, &expected, true)?;
                    let selected_path = root.join("profile-bindings.json");
                    let selected = fs::read(&selected_path).unwrap();
                    let mut changed = load_binding_history(root)?;
                    changed
                        .bindings
                        .values_mut()
                        .next()
                        .unwrap()
                        .launch_snapshot_sha256 = "c".repeat(64);
                    store_binding_history(root, &changed)?;
                    assert_eq!(
                        verify_binding_maintenance_completion(root, operation, &expected, true)
                            .unwrap_err()
                            .code,
                        "LOOKUP_NAMESPACE_CORRUPT"
                    );
                    // Released admission permits later legitimate bindings while the original receipt remains provable.
                    verify_binding_maintenance_completion(root, operation, &expected, false)?;
                    fs::write(selected_path, selected).unwrap();
                    Ok(())
                })
                .unwrap();
            fs::remove_dir_all(parent).unwrap();
        }
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
    fn oversized_binding_prefix_rejects_known_integrity_errors() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let path = home.root().join("profile-bindings.json");
        for prefix in [
            "{\"schema_version\":2,",
            "{\"schema_version\":\"1\",",
            "{\"schema_version\":1,\"schema_version\":1,",
            "{\"unknown\":0,",
            "{\"schema_version\":1,\"bindings\":{\"invalid\":",
        ] {
            let mut bytes = prefix.as_bytes().to_vec();
            bytes.resize(MAX_BINDING_HISTORY_BYTES as usize + 1, b' ');
            bytes.extend_from_slice(b"\"bindings\":{}}");
            fs::write(&path, &bytes).unwrap();
            assert_eq!(
                load_binding_history(home.root()).unwrap_err().code,
                "LEGACY_STATE_UNSUPPORTED",
                "{prefix}"
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        let mut valid_prefix = b"{\"schema_version\":1,".to_vec();
        valid_prefix.resize(MAX_BINDING_HISTORY_BYTES as usize + 1, b' ');
        valid_prefix.extend_from_slice(b"\"bindings\":{}}");
        fs::write(&path, &valid_prefix).unwrap();
        assert_eq!(
            load_binding_history(home.root()).unwrap_err().code,
            "STORAGE_CAPACITY_EXCEEDED"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn persisted_file_limits_accept_the_boundary_and_reject_without_mutation() {
        for (name, maximum) in [
            ("state.json", MAX_HOME_STATE_BYTES),
            ("profiles.yaml", MAX_REGISTRY_BYTES),
            ("profile-bindings.json", MAX_BINDING_HISTORY_BYTES),
        ] {
            let (parent, home) = test_home();
            initialize_generation(&home).unwrap();
            let path = home.root().join(name);
            let original = fs::read(&path).unwrap();
            for length in [maximum - 1, maximum, maximum + 1] {
                let mut bytes = original.clone();
                bytes.resize(usize::try_from(length).unwrap(), b' ');
                fs::write(&path, &bytes).unwrap();
                let result = GlobalProfileStore::new(&home).load();
                if length <= maximum {
                    assert!(result.is_ok(), "{name} at {length}: {result:?}");
                    assert!(load_registry_for_removal(home.root()).is_ok());
                } else {
                    let error = result.unwrap_err();
                    if name == "profiles.yaml" {
                        assert_eq!(error.code, "PROFILE_CONFIG_INVALID");
                        assert_eq!(
                            load_registry_for_removal(home.root()).unwrap_err().code,
                            "PROFILE_CONFIG_INVALID"
                        );
                    } else if name == "profile-bindings.json" {
                        assert_eq!(error.code, "STORAGE_CAPACITY_EXCEEDED");
                    } else {
                        assert_eq!(error.code, "LEGACY_STATE_UNSUPPORTED");
                        assert_eq!(error.details["classification"], "malformed_marker");
                    }
                }
                assert_eq!(fs::read(&path).unwrap(), bytes);
            }
            fs::remove_dir_all(parent).unwrap();
        }
    }

    #[test]
    fn oversized_file_reads_stop_after_the_overflow_byte() {
        let (parent, home) = test_home();
        initialize_generation(&home).unwrap();
        let marker = home.root().join("state.json");
        File::options()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
        let prefix = read_prefix(&marker, MAX_HOME_STATE_BYTES).unwrap();
        assert_eq!(prefix.len() as u64, MAX_HOME_STATE_BYTES + 1);
        assert!(prefix.starts_with(HOME_STATE_BYTES));
        assert_eq!(
            inspect_generation(&home).unwrap_err().details["classification"],
            "malformed_marker"
        );
        assert_eq!(fs::metadata(&marker).unwrap().len(), 64 * 1024 * 1024);
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
        fs::write(&marker, [0xff]).unwrap();
        assert_eq!(
            inspect_generation(&home).unwrap_err().details["classification"],
            "malformed_marker"
        );
        assert_eq!(fs::read(&marker).unwrap(), [0xff]);
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
