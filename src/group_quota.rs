//! Durable input-capture charges owned by actual execution member Runs.
use crate::artifact::{ArtifactQuotaLease, RunQuotaSerializationLease};
use crate::delegation_identity::BytesRef;
use crate::machine::MachineError;
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub(crate) struct InputCharge {
    pub entry_key: String,
    pub run_id: Uuid,
    pub content: BytesRef,
    pub new_run: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CaptureCharge {
    schema: String,
    group_ref: Uuid,
    entry_key: String,
    run_id: Uuid,
    content: BytesRef,
}
impl CaptureCharge {
    fn key(&self) -> String {
        format!(
            "group-capture-quota:v1:{}:{}:{}",
            self.run_id,
            self.group_ref,
            crate::jcs::sha256_hex(self.entry_key.as_bytes())
        )
    }
    fn valid(&self) -> bool {
        self.schema == "dolgorae.group-capture-quota/v1"
            && self.group_ref.get_version_num() == 7
            && self.run_id.get_version_num() == 7
            && !self.entry_key.is_empty()
            && self.entry_key.len() <= 256
            && self.content.byte_length <= 128 * 1024 * 1024
            && self
                .content
                .sha256
                .strip_prefix("sha256:")
                .is_some_and(|digest| {
                    digest.len() == 64
                        && digest
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
    }
}

fn integrity() -> MachineError {
    MachineError::new(
        "ARTIFACT_INTEGRITY_FAILURE",
        "Group capture quota evidence is invalid",
        false,
        json!({}),
    )
}
fn capacity(run: Uuid) -> MachineError {
    MachineError::new(
        "ARTIFACT_CAPACITY_EXCEEDED",
        "Run artifact logical capacity cannot admit group captures",
        false,
        json!({"run_id":run}),
    )
}

fn records(
    connection: &Connection,
    run: Uuid,
) -> Result<BTreeMap<String, CaptureCharge>, MachineError> {
    crate::engagement::group_capture_quota_records(connection, run)?
        .into_iter()
        .map(|(key, value)| {
            let record: CaptureCharge = serde_json::from_str(&value).map_err(|_| integrity())?;
            if !record.valid() || record.run_id != run || record.key() != key {
                return Err(integrity());
            }
            Ok((key, record))
        })
        .collect()
}

pub(crate) fn reserved_bytes_on(connection: &Connection, run: Uuid) -> Result<u64, MachineError> {
    records(connection, run)?
        .values()
        .try_fold(0_u64, |sum, record| {
            sum.checked_add(record.content.byte_length)
                .ok_or_else(integrity)
        })
}
pub(crate) fn reserved_bytes(state_root: &Path, run: Uuid) -> Result<u64, MachineError> {
    let store = crate::engagement::EngagementStore::open_read_only(
        &crate::engagement::EngagementStore::workspace_database_path(state_root),
    )?;
    reserved_bytes_on(store.connection(), run)
}

/// Advisory placement hint for an existing Run. Concurrent publication may
/// consume this headroom; only `GroupQuotaPlan::prepare` reserves capacity.
pub(crate) fn available_bytes(state_root: &Path, run: Uuid) -> Result<u64, MachineError> {
    let root = crate::run::run_root(state_root, run);
    crate::workspace::verify_secure_directory(&root, crate::darwin::DarwinSystem.current_uid())?;
    let used = crate::artifact::logical_artifact_bytes(&root.join("artifacts"))?;
    Ok(crate::artifact::MAX_RUN_ARTIFACT_BYTES.saturating_sub(used))
}

struct RunLocks {
    run_id: Uuid,
    serialization: RunQuotaSerializationLease,
    _physical: Option<ArtifactQuotaLease>,
    existing: BTreeMap<String, CaptureCharge>,
}

pub(crate) struct GroupQuotaPlan {
    state_root: PathBuf,
    charges: Vec<CaptureCharge>,
    locks: Vec<RunLocks>,
}
impl GroupQuotaPlan {
    /// Acquire all actual-Run locks in UUID order before the owning write transaction.
    pub(crate) fn prepare(
        state_root: &Path,
        group_ref: Uuid,
        inputs: Vec<InputCharge>,
    ) -> Result<Self, MachineError> {
        if inputs.is_empty() || inputs.len() > 64 {
            return Err(integrity());
        }
        let mut entries = BTreeSet::new();
        let mut owners = BTreeMap::new();
        let mut charges = Vec::new();
        for input in inputs {
            let charge = CaptureCharge {
                schema: "dolgorae.group-capture-quota/v1".to_owned(),
                group_ref,
                entry_key: input.entry_key,
                run_id: input.run_id,
                content: input.content,
            };
            if !charge.valid() || !entries.insert(charge.entry_key.clone()) {
                return Err(integrity());
            }
            if owners
                .insert(charge.run_id, input.new_run)
                .is_some_and(|old| old != input.new_run)
            {
                return Err(integrity());
            }
            charges.push(charge);
        }
        let mut locks = Vec::new();
        for (run_id, new_run) in owners {
            let serialization = RunQuotaSerializationLease::acquire(state_root, run_id)?;
            let runs = state_root.join("runs");
            match std::fs::symlink_metadata(&runs) {
                Ok(_) => crate::workspace::verify_secure_directory(
                    &runs,
                    crate::darwin::DarwinSystem.current_uid(),
                )?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && new_run => {}
                _ => return Err(integrity()),
            }
            let root = crate::run::run_root(state_root, run_id);
            let mut physical = None;
            let store = crate::engagement::EngagementStore::open_read_only(
                &crate::engagement::EngagementStore::workspace_database_path(state_root),
            )?;
            let existing = records(store.connection(), run_id)?;
            let usage = match std::fs::symlink_metadata(&root) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && new_run => {
                    reserved_bytes_on(store.connection(), run_id)?
                }
                Ok(_) if !new_run => {
                    crate::workspace::verify_secure_directory(
                        &root,
                        crate::darwin::DarwinSystem.current_uid(),
                    )?;
                    let artifacts = root.join("artifacts");
                    match crate::workspace::create_directory(&artifacts, 0o700) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(_) => return Err(integrity()),
                    }
                    physical = Some(ArtifactQuotaLease::acquire_under_reservation(
                        &artifacts,
                        &serialization,
                    )?);
                    crate::artifact::logical_artifact_bytes(&artifacts)?
                }
                _ => return Err(integrity()),
            };
            let extra = charges
                .iter()
                .filter(|charge| charge.run_id == run_id)
                .try_fold(0_u64, |sum, charge| {
                    if let Some(old) = existing.get(&charge.key()) {
                        if old != charge {
                            return Err(integrity());
                        }
                        Ok(sum)
                    } else {
                        sum.checked_add(charge.content.byte_length)
                            .ok_or_else(integrity)
                    }
                })?;
            if usage
                .checked_add(extra)
                .is_none_or(|total| total > crate::artifact::MAX_RUN_ARTIFACT_BYTES)
            {
                return Err(capacity(run_id));
            }
            locks.push(RunLocks {
                run_id,
                serialization,
                _physical: physical,
                existing,
            });
        }
        Ok(Self {
            state_root: state_root.to_owned(),
            charges,
            locks,
        })
    }

    /// Commit alongside captures, exact member-birth reservations and the receipt.
    /// A failed surrounding transaction leaves no capture charge.
    pub(crate) fn commit_in(&self, transaction: &Transaction<'_>) -> Result<(), MachineError> {
        for lock in &self.locks {
            lock.serialization.verify(&self.state_root, lock.run_id)?;
            if records(transaction, lock.run_id)? != lock.existing {
                return Err(integrity());
            }
        }
        for charge in &self.charges {
            let key = charge.key();
            let encoded = serde_json::to_string(charge).map_err(|_| integrity())?;
            let old: Option<String> = transaction
                .query_row("SELECT value FROM metadata WHERE key=?1", [&key], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(|_| integrity())?;
            if let Some(old) = old {
                if serde_json::from_str::<CaptureCharge>(&old).map_err(|_| integrity())? != *charge
                {
                    return Err(integrity());
                }
            } else {
                transaction
                    .execute(
                        "INSERT INTO metadata(key,value) VALUES(?1,?2)",
                        rusqlite::params![key, encoded],
                    )
                    .map_err(|_| integrity())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("group-quota-{}", Uuid::now_v7()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            drop(
                crate::engagement::EngagementStore::open(
                    &crate::engagement::EngagementStore::workspace_database_path(&root),
                )
                .unwrap(),
            );
            Self { root }
        }
        fn store(&self) -> crate::engagement::EngagementStore {
            crate::engagement::EngagementStore::open(
                &crate::engagement::EngagementStore::workspace_database_path(&self.root),
            )
            .unwrap()
        }
        fn birth(&self, run: Uuid) -> PathBuf {
            let runs = self.root.join("runs");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&runs)
                .unwrap();
            let root = crate::run::run_root(&self.root, run);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            let artifacts = root.join("artifacts");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&artifacts)
                .unwrap();
            artifacts
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn input(run: Uuid, key: &str, bytes: u64, new_run: bool) -> InputCharge {
        InputCharge {
            entry_key: key.to_owned(),
            run_id: run,
            content: BytesRef {
                byte_length: bytes,
                sha256: format!("sha256:{}", "a".repeat(64)),
            },
            new_run,
        }
    }
    #[test]
    fn reservations_rollback_replay_accumulate_and_survive_birth() {
        let fixture = Fixture::new();
        let run = Uuid::now_v7();
        let group = Uuid::now_v7();
        let first = GroupQuotaPlan::prepare(
            &fixture.root,
            group,
            vec![input(run, "one", 100, true), input(run, "two", 200, true)],
        )
        .unwrap();
        let mut store = fixture.store();
        let rejected: Result<(), MachineError> = store.transact(|tx| {
            first.commit_in(tx)?;
            tx.execute(
                "INSERT INTO metadata(key,value) VALUES('quota-test-receipt','accepted')",
                [],
            )
            .unwrap();
            Err(integrity())
        });
        assert!(rejected.is_err());
        assert_eq!(reserved_bytes(&fixture.root, run).unwrap(), 0);
        assert!(!crate::run::run_root(&fixture.root, run).exists());
        store.transact(|tx| first.commit_in(tx)).unwrap();
        drop(first);
        let replay = GroupQuotaPlan::prepare(
            &fixture.root,
            group,
            vec![input(run, "one", 100, true), input(run, "two", 200, true)],
        )
        .unwrap();
        store.transact(|tx| replay.commit_in(tx)).unwrap();
        drop(replay);
        assert_eq!(reserved_bytes(&fixture.root, run).unwrap(), 300);
        let artifacts = fixture.birth(run);
        let lease = ArtifactQuotaLease::acquire(&artifacts).unwrap();
        assert_eq!(
            crate::artifact::logical_artifact_bytes(&artifacts).unwrap(),
            300
        );
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(artifacts.join(format!("{}.bin", Uuid::now_v7())))
            .unwrap()
            .set_len(9)
            .unwrap();
        assert_eq!(
            crate::artifact::logical_artifact_bytes(&artifacts).unwrap(),
            309
        );
        drop(lease);
        let later = GroupQuotaPlan::prepare(
            &fixture.root,
            Uuid::now_v7(),
            vec![input(run, "three", 10, false)],
        )
        .unwrap();
        store.transact(|tx| later.commit_in(tx)).unwrap();
        drop(later);
        assert_eq!(
            crate::artifact::logical_artifact_bytes(&artifacts).unwrap(),
            319
        );
        assert_eq!(
            available_bytes(&fixture.root, run).unwrap(),
            crate::artifact::MAX_RUN_ARTIFACT_BYTES - 319
        );
    }
    #[test]
    fn cumulative_runs_refuse_capacity_without_reserving_more() {
        let fixture = Fixture::new();
        let run = Uuid::now_v7();
        let plan = GroupQuotaPlan::prepare(
            &fixture.root,
            Uuid::now_v7(),
            vec![
                input(run, "a", 128 * 1024 * 1024, true),
                input(run, "b", 128 * 1024 * 1024, true),
            ],
        )
        .unwrap();
        fixture.store().transact(|tx| plan.commit_in(tx)).unwrap();
        drop(plan);
        let refused = GroupQuotaPlan::prepare(
            &fixture.root,
            Uuid::now_v7(),
            vec![input(run, "overflow", 1, true)],
        )
        .err()
        .unwrap();
        assert_eq!(refused.code, "ARTIFACT_CAPACITY_EXCEEDED");
        assert_eq!(
            reserved_bytes(&fixture.root, run).unwrap(),
            256 * 1024 * 1024
        );
        assert!(!crate::run::run_root(&fixture.root, run).exists());
    }
    #[test]
    fn capture_reservations_hold_the_same_lock_as_run_publishers() {
        let fixture = Fixture::new();
        let run = Uuid::now_v7();
        let artifacts = fixture.birth(run);
        let plan = GroupQuotaPlan::prepare(
            &fixture.root,
            Uuid::now_v7(),
            vec![input(run, "one", 9, false)],
        )
        .unwrap();
        let path = fixture
            .root
            .join("artifact-quota")
            .join(format!("{run}.lock"));
        let contender = std::fs::File::open(&path).unwrap();
        assert_eq!(
            crate::darwin::DarwinSystem
                .lock_exclusive_nonblocking(&contender)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(plan);
        let lease = ArtifactQuotaLease::acquire(&artifacts).unwrap();
        assert_eq!(
            crate::darwin::DarwinSystem
                .lock_exclusive_nonblocking(&contender)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(lease);
        crate::darwin::DarwinSystem
            .lock_exclusive_nonblocking(&contender)
            .unwrap();
        crate::darwin::DarwinSystem.unlock(&contender).unwrap();
    }
}
