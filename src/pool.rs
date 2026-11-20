//! Successor admission bounds. Accepted Run stores remain the usage authority.
use crate::darwin::DarwinSystem;
use crate::domain::{ExecutionLane, RunLifecycle};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::run::RunStore;
use crate::workspace::{SystemWorkspacePlatform, verify_secure_directory, verify_secure_file};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfiguration {
    pub schema: String,
    pub readonly_server_limit: u32,
    pub dedicated_generation_cap: u32,
    pub sixth_generation_warning: u32,
    pub native_subagent_default: String,
}
impl Default for PoolConfiguration {
    fn default() -> Self {
        Self {
            schema: "dolgorae.pool-configuration/v1".into(),
            readonly_server_limit: 1,
            dedicated_generation_cap: 8,
            sixth_generation_warning: 6,
            native_subagent_default: "disabled".into(),
        }
    }
}
impl PoolConfiguration {
    pub fn validate(&self) -> Result<(), MachineError> {
        if self.schema != "dolgorae.pool-configuration/v1"
            || !(1..=16).contains(&self.readonly_server_limit)
            || !(1..=32).contains(&self.dedicated_generation_cap)
            || self.sixth_generation_warning != 6
            || self.native_subagent_default != "disabled"
        {
            return Err(MachineError::invalid_argument(
                "pool_configuration",
                "invalid pool bounds or policy",
            ));
        }
        Ok(())
    }
    pub fn read(home: &DolgoraeHome) -> Result<Self, MachineError> {
        let path = home.root().join("pool-configuration.json");
        let Some(file) =
            crate::workspace::open_secure_file_if_present(&path, DarwinSystem.current_uid())
                .map_err(|_| invalid())?
        else {
            return Ok(Self::default());
        };
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > 4096 {
            return Err(invalid());
        }
        let config: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        config.validate()?;
        Ok(config)
    }
    pub fn admit_dedicated(&self, used: u32) -> Result<bool, MachineError> {
        self.validate()?;
        if used >= self.dedicated_generation_cap {
            return Err(MachineError::new(
                "POOL_CAPACITY_EXCEEDED",
                "dedicated generation capacity is exhausted",
                true,
                serde_json::json!({"resource":"dedicated_generations", "limit":self.dedicated_generation_cap, "current":used}),
            ));
        }
        Ok(used + 1 == self.sixth_generation_warning)
    }
}

/// Serializes new admission until its accepted manifest and Worker are visible.
/// Recovery, finalization and close never acquire a capacity reservation.
pub(crate) struct PoolAdmission {
    _lock: File,
    pub sixth_generation_warning: bool,
}
impl PoolAdmission {
    pub(crate) fn acquire(
        home: &DolgoraeHome,
        profile: &str,
        lane: ExecutionLane,
    ) -> Result<Self, MachineError> {
        verify_secure_directory(home.root(), DarwinSystem.current_uid()).map_err(|_| invalid())?;
        let path = home.root().join("pool-admission.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| invalid())?;
        verify_secure_file(&path, DarwinSystem.current_uid()).map_err(|_| invalid())?;
        DarwinSystem.lock_exclusive(&lock).map_err(|_| invalid())?;
        let config = PoolConfiguration::read(home)?;
        let mut used = 0;
        let roots = fs::read_dir(home.root().join("workspaces")).map_err(|_| invalid())?;
        for workspace in roots {
            let root = workspace.map_err(|_| invalid())?.path();
            verify_secure_directory(&root, DarwinSystem.current_uid()).map_err(|_| invalid())?;
            let runs = root.join("runs");
            let store = RunStore::new(SystemWorkspacePlatform, &root);
            let mut published = std::collections::BTreeSet::new();
            let entries = match fs::read_dir(&runs) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(invalid()),
            };
            for run in entries.into_iter().flatten() {
                let run = run.map_err(|_| invalid())?;
                let Some(id) = run
                    .file_name()
                    .to_str()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                else {
                    continue;
                };
                if store.deletion(id)?.is_some() {
                    published.insert(id);
                    continue;
                }
                let manifest = store.load_manifest(id)?;
                published.insert(id);
                if manifest.profile.profile_name != profile
                    || manifest.execution_lane != ExecutionLane::Dedicated
                {
                    continue;
                }
                let state = store.load_state_projection(id)?;
                if matches!(state.lifecycle, RunLifecycle::Closed | RunLifecycle::Paused) {
                    continue;
                }
                // Threadless legacy allocation owns no physical generation.
                if manifest.schema_version <= 2 && state.thread_id.is_none() {
                    continue;
                }
                used += 1;
            }
            for (id, selected, lane) in
                crate::run::StartReservationStore::new(SystemWorkspacePlatform, &root)
                    .accepted_caller_capacity()?
            {
                if !published.contains(&id)
                    && selected == profile
                    && lane == ExecutionLane::Dedicated
                {
                    used += 1;
                }
            }
        }
        let warning = lane == ExecutionLane::Dedicated && config.admit_dedicated(used)?;
        Ok(Self {
            _lock: lock,
            sixth_generation_warning: warning,
        })
    }
}
fn invalid() -> MachineError {
    MachineError::config_invalid(
        "pool_configuration",
        "pool configuration or usage authority is unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_refusal_is_typed_and_sixth_warning_precedes_effects() {
        let mut config = PoolConfiguration::default();
        assert!(!config.admit_dedicated(4).unwrap());
        assert!(config.admit_dedicated(5).unwrap());
        assert_eq!(
            config.admit_dedicated(8).unwrap_err().code,
            "POOL_CAPACITY_EXCEEDED"
        );
        config.dedicated_generation_cap = 2;
        assert_eq!(
            config.admit_dedicated(5).unwrap_err().code,
            "POOL_CAPACITY_EXCEEDED"
        );
        for k in [0, 17] {
            config.readonly_server_limit = k;
            assert!(config.validate().is_err());
        }
    }
}
