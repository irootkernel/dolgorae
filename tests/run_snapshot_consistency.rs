//! Verified display prefixes remain separate from fresh complete-history proof.
use dolgorae::audit::{AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH};
use dolgorae::jcs::LosslessJson;
use dolgorae::ledger::{LedgerError, ObservedLedger, observation_counters};
use std::fs::{self, OpenOptions};
use std::io::{Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use uuid::Uuid;

fn isolated_counter_process() -> bool {
    let name = std::thread::current().name().unwrap().to_owned();
    if std::env::var("DOLGORAE_SNAPSHOT_COUNTER_TEST").as_deref() == Ok(name.as_str()) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env("DOLGORAE_SNAPSHOT_COUNTER_TEST", &name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

struct History {
    root: PathBuf,
    run_id: Uuid,
    records: Vec<AuditRecord>,
}
impl History {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("dolgorae-display-{}", Uuid::now_v7()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        Self {
            root,
            run_id: Uuid::now_v7(),
            records: Vec::new(),
        }
    }
    fn append(&mut self) {
        let record = AuditRecord::new(
            self.records.len() as u64 + 1,
            "2026-10-14T12:00:00.000000Z",
            self.run_id,
            1,
            AuditKind::RunCreated,
            LosslessJson::Object(vec![]),
            self.records
                .last()
                .map_or(GENESIS_PREVIOUS_HASH, AuditRecord::hash),
        )
        .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(self.root.join("audit.jsonl"))
            .unwrap();
        file.write_all(&record.canonical_line().unwrap()).unwrap();
        file.sync_all().unwrap();
        self.records.push(record);
    }
    fn bytes(&self, head: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        ObservedLedger::open_display(&self.root, self.run_id, head)
            .unwrap()
            .write_canonical_history(&mut bytes)
            .unwrap();
        bytes
    }
}
impl Drop for History {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn published_suffix_is_verified_once_and_idle_readers_share_the_prefix() {
    if isolated_counter_process() {
        return;
    }
    let mut history = History::new();
    history.append();
    let first = history.bytes(1);
    let baseline = observation_counters();
    for _ in 0..40 {
        assert_eq!(history.bytes(1), first);
    }
    history.append();
    let second = history.bytes(2);
    assert_eq!(
        history.bytes(1),
        first,
        "an older captured head cannot expose a newer record"
    );
    assert_eq!(
        second,
        history
            .records
            .iter()
            .flat_map(|record| record.canonical_line().unwrap())
            .collect::<Vec<_>>()
    );
    let after = observation_counters();
    assert_eq!(after.full_replays - baseline.full_replays, 0);
    assert_eq!(after.tail_records - baseline.tail_records, 1);
    assert!(after.cache_hits - baseline.cache_hits >= 41);
    let root = history.root.clone();
    let run_id = history.run_id;
    std::thread::scope(|scope| {
        for _ in 0..6 {
            let root = &root;
            let second = &second;
            scope.spawn(move || {
                let mut observed = Vec::new();
                ObservedLedger::open_display(root, run_id, 2)
                    .unwrap()
                    .write_canonical_history(&mut observed)
                    .unwrap();
                assert_eq!(&observed, second);
            });
        }
    });
    assert_eq!(observation_counters().full_replays, after.full_replays);
}

#[test]
fn unpublished_tail_never_becomes_authoritative_absence_evidence() {
    if isolated_counter_process() {
        return;
    }
    let mut history = History::new();
    history.append();
    let published = history.bytes(1);
    let mut file = OpenOptions::new()
        .append(true)
        .open(history.root.join("audit.jsonl"))
        .unwrap();
    file.write_all(b"{\"in_flight\":").unwrap();
    file.sync_all().unwrap();
    assert_eq!(history.bytes(1), published);
    assert!(matches!(
        ObservedLedger::open_complete(&history.root, history.run_id, 1),
        Err(LedgerError::CaptureConflict)
    ));
    assert!(ObservedLedger::open_display(&history.root, history.run_id, 2).is_err());
    assert_eq!(history.bytes(1), published);
}

#[test]
fn rewrite_truncation_replacement_and_security_changes_cannot_reuse_a_valid_prefix() {
    if isolated_counter_process() {
        return;
    }
    let mut history = History::new();
    history.append();
    let published = history.bytes(1);
    let path = history.root.join("audit.jsonl");
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"!").unwrap();
    file.sync_all().unwrap();
    assert!(ObservedLedger::open_display(&history.root, history.run_id, 1).is_err());
    fs::write(&path, &published).unwrap();
    assert_eq!(history.bytes(1), published);
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(0)
        .unwrap();
    assert!(ObservedLedger::open_display(&history.root, history.run_id, 1).is_err());
    fs::write(&path, &published).unwrap();
    assert_eq!(history.bytes(1), published);
    fs::rename(&path, history.root.join("old.jsonl")).unwrap();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    assert!(ObservedLedger::open_display(&history.root, history.run_id, 1).is_err());
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(history.root.join("old.jsonl"), &path).unwrap();
    assert!(ObservedLedger::open_display(&history.root, history.run_id, 1).is_err());
    fs::remove_file(&path).unwrap();
    fs::rename(history.root.join("old.jsonl"), &path).unwrap();
    assert_eq!(history.bytes(1), published);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(ObservedLedger::open_display(&history.root, history.run_id, 1).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(history.bytes(1), published);
}
