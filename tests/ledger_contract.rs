use dolgorae::audit::{AuditKind, AuditRecord};
use dolgorae::fault::{FaultBarrier, FaultInjected, FaultInjector, NoFaults};
use dolgorae::jcs::{LosslessJson, sha256_hex};
use dolgorae::ledger::{
    AppendDurability, ArtifactMetadata, ClientEventData, ClientEventRecord, EventProjection,
    FinalResponse, Ledger, LedgerClock, LedgerError, MAX_AUDIT_LINE_BYTES, MAX_EVENT_DELIVERIES,
    MAX_STATE_BYTES, ResponseEventPayload, RunStateProjection, RuntimeErrorPayload, UsagePayload,
    WorkspaceChangesPayload,
};
use dolgorae::workspace::LosslessPath;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const TIMESTAMP: &str = "2026-08-21T12:34:56.123456Z";

#[derive(Clone)]
struct TestClock {
    millis: Arc<AtomicU64>,
}

impl TestClock {
    fn new() -> Self {
        Self {
            millis: Arc::new(AtomicU64::new(0)),
        }
    }

    fn advance(&self, millis: u64) {
        self.millis.fetch_add(millis, Ordering::SeqCst);
    }
}

impl LedgerClock for TestClock {
    fn monotonic_millis(&self) -> u64 {
        self.millis.load(Ordering::SeqCst)
    }

    fn timestamp(&self) -> String {
        TIMESTAMP.to_owned()
    }
}

#[derive(Clone, Default)]
struct RecordingFaults {
    seen: Arc<Mutex<Vec<FaultBarrier>>>,
    fail: Arc<Mutex<Option<FaultBarrier>>>,
}

impl RecordingFaults {
    fn failing(barrier: FaultBarrier) -> Self {
        Self {
            seen: Arc::default(),
            fail: Arc::new(Mutex::new(Some(barrier))),
        }
    }
}

impl FaultInjector for RecordingFaults {
    fn check(&self, barrier: FaultBarrier) -> Result<(), FaultInjected> {
        self.seen.lock().unwrap().push(barrier);
        let mut fail = self.fail.lock().unwrap();
        if fail.as_ref() == Some(&barrier) {
            *fail = None;
            Err(FaultInjected(barrier))
        } else {
            Ok(())
        }
    }
}

#[test]
fn streaming_group_commit_publishes_at_100_milliseconds_only_after_fsync() {
    let tree = RunTree::new();
    let clock = TestClock::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, clock.clone(), NoFaults).unwrap();
    let record = next_record(
        &ledger,
        AuditKind::RunCreated,
        object(&[
            ("initial_access", string("read")),
            ("default_effort", string("high")),
        ]),
    );
    ledger.append(record, AppendDurability::Streaming).unwrap();
    assert_eq!(read_state(&tree.root).ledger_head.sequence, 0);
    assert_eq!(ledger.durable_records().len(), 0);
    clock.advance(99);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(read_state(&tree.root).ledger_head.sequence, 0);
    clock.advance(1);
    wait_until(Duration::from_millis(500), || {
        read_state(&tree.root).ledger_head.sequence == 1
    });
    assert_eq!(ledger.durable_records().len(), 1);
    assert_eq!(read_state(&tree.root).ledger_head.sequence, 1);
    assert_eq!(
        ledger.projection().access,
        dolgorae::ledger::ProjectedAccess::Read
    );
}

#[test]
fn failed_background_commit_poison_is_retained_and_drop_retries_pending_work() {
    let tree = RunTree::new();
    let clock = TestClock::new();
    let faults = RecordingFaults::failing(FaultBarrier::BeforeLedgerFileSync);
    let mut ledger = Ledger::open_with(
        tree.root.clone(),
        tree.run_id,
        clock.clone(),
        faults.clone(),
    )
    .unwrap();
    let record = next_record(&ledger, AuditKind::RunCreated, object(&[]));
    ledger.append(record, AppendDurability::Streaming).unwrap();
    clock.advance(100);
    wait_until(Duration::from_millis(500), || {
        faults
            .seen
            .lock()
            .unwrap()
            .contains(&FaultBarrier::BeforeLedgerFileSync)
    });
    thread::sleep(Duration::from_millis(10));
    assert!(ledger.flush().is_err());
    drop(ledger);

    let reopened =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(reopened.durable_records().len(), 1);
    assert_eq!(read_state(&tree.root).ledger_head.sequence, 1);
}

#[test]
fn dropping_a_streaming_ledger_cannot_rewind_a_reopened_projection() {
    let tree = RunTree::new();
    let old_clock = TestClock::new();
    let mut old =
        Ledger::open_with(tree.root.clone(), tree.run_id, old_clock.clone(), NoFaults).unwrap();
    let first = next_record(&old, AuditKind::RunCreated, object(&[]));
    old.append(first, AppendDurability::Streaming).unwrap();
    drop(old);

    let mut reopened =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    let second = next_record(
        &reopened,
        AuditKind::ThreadBound,
        object(&[("thread_id", string("thread-1"))]),
    );
    reopened.append(second, AppendDurability::Required).unwrap();
    assert_eq!(read_state(&tree.root).ledger_head.sequence, 2);

    old_clock.advance(100);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(read_state(&tree.root).ledger_head.sequence, 2);
}

#[test]
fn write_ahead_effect_runs_only_after_durable_record_and_projection() {
    let tree = RunTree::new();
    let clock = TestClock::new();
    let faults = RecordingFaults::default();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, clock, faults.clone()).unwrap();
    let record = next_record(
        &ledger,
        AuditKind::CleanupIntent,
        object(&[("reason", string("test"))]),
    );
    ledger
        .append_before_effect(record, || {
            assert_eq!(read_state(&tree.root).ledger_head.sequence, 1);
            Ok(())
        })
        .unwrap();
    let seen = faults.seen.lock().unwrap();
    let sync = seen
        .iter()
        .position(|barrier| *barrier == FaultBarrier::AfterLedgerFileSync)
        .unwrap();
    let effect = seen
        .iter()
        .position(|barrier| *barrier == FaultBarrier::BeforeExternalEffect)
        .unwrap();
    assert!(sync < effect);
}

#[test]
fn fault_before_effect_never_executes_the_effect_but_leaves_durable_intent() {
    let tree = RunTree::new();
    let clock = TestClock::new();
    let faults = RecordingFaults::failing(FaultBarrier::BeforeExternalEffect);
    let mut ledger = Ledger::open_with(tree.root.clone(), tree.run_id, clock, faults).unwrap();
    let record = next_record(
        &ledger,
        AuditKind::TurnIntent,
        object(&[("turn_id", string("turn-1"))]),
    );
    let called = AtomicBool::new(false);
    assert!(
        ledger
            .append_before_effect(record, || {
                called.store(true, Ordering::SeqCst);
                Ok(())
            })
            .is_err()
    );
    assert!(!called.load(Ordering::SeqCst));
    drop(ledger);
    let reopened =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(reopened.durable_records().len(), 1);
}

#[test]
fn torn_tail_is_preserved_repaired_and_idempotent() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    let record = next_record(&ledger, AuditKind::RunCreated, object(&[]));
    ledger.append(record, AppendDurability::Required).unwrap();
    drop(ledger);
    let tail = br#"{"complete":"json-without-newline"}"#;
    OpenOptions::new()
        .append(true)
        .open(tree.root.join("audit.jsonl"))
        .unwrap()
        .write_all(tail)
        .unwrap();

    let repaired =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(repaired.durable_records().len(), 2);
    assert_eq!(
        repaired.durable_records()[1].kind(),
        AuditKind::LedgerTailRepaired
    );
    let expected = format!("tail-2-{}.bin", sha256_hex(tail));
    assert_eq!(
        fs::read(tree.root.join("recovery").join(expected)).unwrap(),
        tail
    );
    drop(repaired);
    let again =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(again.durable_records().len(), 2);
}

#[test]
fn stale_atomic_evidence_temporary_is_removed_during_repair() {
    let tree = RunTree::new();
    let temporary = tree.root.join("recovery/.tail-evidence-stale.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .unwrap();
    file.write_all(b"partial temporary evidence").unwrap();
    OpenOptions::new()
        .append(true)
        .open(tree.root.join("audit.jsonl"))
        .unwrap()
        .write_all(b"torn")
        .unwrap();

    let repaired =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(repaired.durable_records().len(), 1);
    assert!(!temporary.exists());
    assert_eq!(fs::read_dir(tree.root.join("recovery")).unwrap().count(), 1);
}

#[test]
fn secure_storage_rejections_remain_typed() {
    let tree = RunTree::new();
    fs::set_permissions(
        tree.root.join("audit.jsonl"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let error = Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults)
        .err()
        .unwrap();
    assert!(matches!(error, LedgerError::SecurityPolicy(_)));
}

#[test]
fn a_torn_repair_record_is_repaired_once_without_sequence_collision() {
    let tree = RunTree::new();
    let original = b"original torn tail";
    let original_name = format!("tail-1-{}.bin", sha256_hex(original));
    let original_path = tree.root.join("recovery").join(original_name);
    let mut evidence = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(original_path)
        .unwrap();
    evidence.write_all(original).unwrap();
    evidence.sync_all().unwrap();
    OpenOptions::new()
        .append(true)
        .open(tree.root.join("audit.jsonl"))
        .unwrap()
        .write_all(br#"{"partial":"ledger_tail_repaired""#)
        .unwrap();

    let repaired =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(repaired.durable_records().len(), 2);
    assert!(
        repaired
            .durable_records()
            .iter()
            .all(|record| record.kind() == AuditKind::LedgerTailRepaired)
    );
    assert_eq!(fs::read_dir(tree.root.join("recovery")).unwrap().count(), 2);
    drop(repaired);

    let reopened =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(reopened.durable_records().len(), 2);
    assert_eq!(fs::read_dir(tree.root.join("recovery")).unwrap().count(), 2);
}

#[test]
fn a_record_that_fails_replay_is_rejected_before_write() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    let invalid = next_record(&ledger, AuditKind::LifecycleTransition, object(&[]));
    assert!(ledger.append(invalid, AppendDurability::Required).is_err());
    assert_eq!(ledger.durable_records().len(), 0);
    ledger.flush().unwrap();
    let valid = next_record(&ledger, AuditKind::RunCreated, object(&[]));
    ledger.append(valid, AppendDurability::Required).unwrap();
    drop(ledger);

    let reopened =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(reopened.durable_records().len(), 1);
}

#[test]
fn oversized_audit_segments_are_rejected_before_tail_repair() {
    let tree = RunTree::new();
    let oversized = vec![b'x'; MAX_AUDIT_LINE_BYTES + 1];
    OpenOptions::new()
        .append(true)
        .open(tree.root.join("audit.jsonl"))
        .unwrap()
        .write_all(&oversized)
        .unwrap();
    let error = Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults)
        .err()
        .unwrap();
    assert!(error.to_string().contains("bounded record limit"));
    assert_eq!(fs::read_dir(tree.root.join("recovery")).unwrap().count(), 0);
}

#[test]
fn invalid_recovery_evidence_and_oversized_projection_fail_closed() {
    let future_tree = RunTree::new();
    let bytes = b"future evidence";
    let future = future_tree
        .root
        .join("recovery")
        .join(format!("tail-9999-{}.bin", sha256_hex(bytes)));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(future)
        .unwrap();
    file.write_all(bytes).unwrap();
    let error = Ledger::open_with(
        future_tree.root.clone(),
        future_tree.run_id,
        TestClock::new(),
        NoFaults,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("future sequence"));

    let mismatch_tree = RunTree::new();
    let mismatch = mismatch_tree.root.join(
        "recovery/tail-1-0000000000000000000000000000000000000000000000000000000000000000.bin",
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(mismatch)
        .unwrap();
    file.write_all(b"wrong digest").unwrap();
    let error = Ledger::open_with(
        mismatch_tree.root.clone(),
        mismatch_tree.run_id,
        TestClock::new(),
        NoFaults,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("digest mismatch"));

    let unknown_tree = RunTree::new();
    let unknown = unknown_tree.root.join("recovery/unrecognized.bin");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(unknown)
        .unwrap();
    let error = Ledger::open_with(
        unknown_tree.root.clone(),
        unknown_tree.run_id,
        TestClock::new(),
        NoFaults,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("unrecognized recovery entry"));

    let oversized_evidence_tree = RunTree::new();
    let oversized_bytes = vec![b'x'; MAX_AUDIT_LINE_BYTES + 1];
    let oversized_evidence = oversized_evidence_tree
        .root
        .join("recovery")
        .join(format!("tail-1-{}.bin", sha256_hex(&oversized_bytes)));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(oversized_evidence)
        .unwrap();
    file.write_all(&oversized_bytes).unwrap();
    let error = Ledger::open_with(
        oversized_evidence_tree.root.clone(),
        oversized_evidence_tree.run_id,
        TestClock::new(),
        NoFaults,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("bounded evidence size"));

    let state_tree = RunTree::new();
    fs::write(
        state_tree.root.join("state.json"),
        vec![b' '; usize::try_from(MAX_STATE_BYTES).unwrap() + 1],
    )
    .unwrap();
    fs::set_permissions(
        state_tree.root.join("state.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let error = Ledger::open_with(
        state_tree.root.clone(),
        state_tree.run_id,
        TestClock::new(),
        NoFaults,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("bounded projection size"));
}

#[test]
fn newline_terminated_middle_corruption_is_never_tail_repaired() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    for kind in [AuditKind::RunCreated, AuditKind::ProfileObserved] {
        let record = next_record(&ledger, kind, object(&[]));
        ledger.append(record, AppendDurability::Required).unwrap();
    }
    drop(ledger);
    let audit = tree.root.join("audit.jsonl");
    let mut bytes = fs::read(&audit).unwrap();
    let position = bytes.iter().position(|byte| *byte == b'0').unwrap();
    bytes[position] = b'1';
    fs::write(&audit, bytes).unwrap();
    let error = Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults)
        .err()
        .unwrap();
    assert!(error.to_string().contains("audit integrity failure"));
    assert_eq!(fs::read_dir(tree.root.join("recovery")).unwrap().count(), 0);
}

#[test]
fn missing_stale_and_ahead_projections_rebuild_from_the_ledger() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    let created = next_record(
        &ledger,
        AuditKind::RunCreated,
        object(&[("initial_access", string("read"))]),
    );
    ledger.append(created, AppendDurability::Required).unwrap();
    let bound = next_record(
        &ledger,
        AuditKind::ThreadBound,
        object(&[("thread_id", string("thread-1"))]),
    );
    ledger.append(bound, AppendDurability::Required).unwrap();
    drop(ledger);

    fs::remove_file(tree.root.join("state.json")).unwrap();
    let rebuilt =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(rebuilt.projection().thread_id.as_deref(), Some("thread-1"));
    drop(rebuilt);

    let stale = RunStateProjection {
        ledger_head: Default::default(),
        thread_id: None,
        ..read_state(&tree.root)
    };
    write_state(&tree.root, &stale);
    let rebuilt_stale =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(rebuilt_stale.durable_records().len(), 2);
    assert_eq!(
        rebuilt_stale.projection().thread_id.as_deref(),
        Some("thread-1")
    );
    drop(rebuilt_stale);

    let mut ahead = read_state(&tree.root);
    ahead.ledger_head.sequence += 5;
    ahead.ledger_head.hash = "f".repeat(64);
    write_state(&tree.root, &ahead);
    let rewound =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    assert_eq!(
        rewound.durable_records().last().unwrap().kind(),
        AuditKind::ProjectionRewound
    );
    assert_eq!(
        read_state(&tree.root).ledger_head.sequence,
        rewound.durable_records().len() as u64
    );
}

#[test]
fn every_ledger_repair_projection_and_effect_crash_barrier_recovers() {
    let repair_barriers = [
        FaultBarrier::BeforeTailEvidenceFileSync,
        FaultBarrier::AfterTailEvidenceFileSync,
        FaultBarrier::BeforeTailEvidenceDirectorySync,
        FaultBarrier::AfterTailEvidenceDirectorySync,
        FaultBarrier::BeforeLedgerTruncate,
        FaultBarrier::AfterLedgerTruncate,
    ];
    for barrier in repair_barriers {
        let tree = RunTree::new();
        OpenOptions::new()
            .append(true)
            .open(tree.root.join("audit.jsonl"))
            .unwrap()
            .write_all(b"torn")
            .unwrap();
        assert!(
            Ledger::open_with(
                tree.root.clone(),
                tree.run_id,
                TestClock::new(),
                RecordingFaults::failing(barrier),
            )
            .is_err()
        );
        let recovered =
            Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
        assert_eq!(recovered.durable_records().len(), 1, "{barrier:?}");
        assert_eq!(
            recovered.durable_records()[0].kind(),
            AuditKind::LedgerTailRepaired
        );
    }

    let append_barriers = [
        FaultBarrier::BeforeLedgerAppend,
        FaultBarrier::AfterLedgerAppend,
        FaultBarrier::BeforeLedgerFileSync,
        FaultBarrier::AfterLedgerFileSync,
    ];
    for barrier in append_barriers {
        let tree = RunTree::new();
        let mut ledger = Ledger::open_with(
            tree.root.clone(),
            tree.run_id,
            TestClock::new(),
            RecordingFaults::failing(barrier),
        )
        .unwrap();
        let record = next_record(&ledger, AuditKind::RunCreated, object(&[]));
        assert!(ledger.append(record, AppendDurability::Required).is_err());
        drop(ledger);
        let recovered =
            Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
        let expected = usize::from(barrier != FaultBarrier::BeforeLedgerAppend);
        assert_eq!(recovered.durable_records().len(), expected, "{barrier:?}");
    }

    let projection_barriers = [
        FaultBarrier::BeforeProjectionReplace,
        FaultBarrier::BeforeProjectionFileSync,
        FaultBarrier::AfterProjectionFileSync,
        FaultBarrier::BeforeProjectionDirectorySync,
        FaultBarrier::AfterProjectionDirectorySync,
    ];
    for barrier in projection_barriers {
        let tree = RunTree::new();
        assert!(
            Ledger::open_with(
                tree.root.clone(),
                tree.run_id,
                TestClock::new(),
                RecordingFaults::failing(barrier),
            )
            .is_err()
        );
        let recovered =
            Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
        assert_eq!(
            recovered.projection().ledger_head.sequence,
            0,
            "{barrier:?}"
        );
    }

    for barrier in [
        FaultBarrier::BeforeExternalEffect,
        FaultBarrier::AfterExternalEffect,
    ] {
        let tree = RunTree::new();
        let mut ledger = Ledger::open_with(
            tree.root.clone(),
            tree.run_id,
            TestClock::new(),
            RecordingFaults::failing(barrier),
        )
        .unwrap();
        let record = next_record(&ledger, AuditKind::CleanupIntent, object(&[]));
        assert!(ledger.append_before_effect(record, || Ok(())).is_err());
        drop(ledger);
        let recovered =
            Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
        assert_eq!(recovered.durable_records().len(), 1, "{barrier:?}");
    }
}

#[test]
fn replay_projects_turn_interaction_writer_and_reconciliation_state() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    for (kind, payload) in [
        (
            AuditKind::RunCreated,
            object(&[("initial_access", string("read"))]),
        ),
        (
            AuditKind::TurnStarted,
            object(&[("turn_id", string("turn-1"))]),
        ),
        (
            AuditKind::InteractionOpened,
            object(&[("request_id", string("request-1"))]),
        ),
        (AuditKind::WriterAcquired, object(&[])),
    ] {
        let record = next_record(&ledger, kind, payload);
        ledger.append(record, AppendDurability::Required).unwrap();
    }
    let waiting = ledger.projection();
    assert_eq!(
        waiting.lifecycle,
        dolgorae::domain::RunLifecycle::WaitingInteraction
    );
    assert_eq!(waiting.pending_requests, ["request-1"]);
    assert_eq!(waiting.active_turn_id.as_deref(), Some("turn-1"));
    assert_eq!(
        waiting.writer_authority,
        dolgorae::ledger::ProjectedWriterAuthority::Active
    );
    assert_eq!(waiting.access, dolgorae::domain::Access::Write);

    for (kind, payload) in [
        (
            AuditKind::InteractionResolved,
            object(&[("request_id", string("request-1"))]),
        ),
        (AuditKind::WriterHandoffRequested, object(&[])),
        (AuditKind::WriterHandoffCompleted, object(&[])),
        (
            AuditKind::TurnTerminal,
            object(&[("turn_id", string("turn-1"))]),
        ),
    ] {
        let record = next_record(&ledger, kind, payload);
        ledger.append(record, AppendDurability::Required).unwrap();
    }
    let final_state = ledger.projection();
    assert_eq!(final_state.lifecycle, dolgorae::domain::RunLifecycle::Idle);
    assert!(final_state.pending_requests.is_empty());
    assert_eq!(final_state.active_turn_id, None);
    assert_eq!(final_state.latest_turn_id.as_deref(), Some("turn-1"));
    assert_eq!(
        final_state.writer_authority,
        dolgorae::ledger::ProjectedWriterAuthority::None
    );
    assert_eq!(final_state.access, dolgorae::domain::Access::Read);

    let reconciliation = next_record(&ledger, AuditKind::Reconciliation, object(&[]));
    ledger
        .append(reconciliation, AppendDurability::Required)
        .unwrap();
    assert_eq!(
        ledger.projection().lifecycle,
        dolgorae::domain::RunLifecycle::ReconciliationRequired
    );
}

#[test]
fn generation_regression_is_rejected_at_append_and_reopen() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    let first = AuditRecord::new(
        1,
        TIMESTAMP,
        tree.run_id,
        2,
        AuditKind::RunGenerationStarted,
        object(&[]),
        ledger.previous_hash(),
    )
    .unwrap();
    ledger.append(first, AppendDurability::Required).unwrap();
    let regressed = AuditRecord::new(
        2,
        TIMESTAMP,
        tree.run_id,
        1,
        AuditKind::RunGenerationStopped,
        object(&[]),
        ledger.previous_hash(),
    )
    .unwrap();
    assert!(
        ledger
            .append(regressed, AppendDurability::Required)
            .is_err()
    );
    drop(ledger);

    let audit = tree.root.join("audit.jsonl");
    let first = fs::read_to_string(&audit).unwrap();
    let parsed = AuditRecord::from_canonical_line(first.trim_end().as_bytes()).unwrap();
    let on_disk = AuditRecord::new(
        2,
        TIMESTAMP,
        tree.run_id,
        1,
        AuditKind::RunGenerationStopped,
        object(&[]),
        parsed.hash(),
    )
    .unwrap();
    OpenOptions::new()
        .append(true)
        .open(&audit)
        .unwrap()
        .write_all(&on_disk.canonical_line().unwrap())
        .unwrap();
    let error = Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults)
        .err()
        .unwrap();
    assert!(error.to_string().contains("regresses run generation"));
}

#[test]
fn client_events_are_validated_at_append_and_share_one_sparse_cursor_domain() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    let usage = event(
        &ledger,
        tree.run_id,
        Some("thread-1"),
        Some("turn-1"),
        ClientEventData::UsageReported(UsagePayload {
            input_tokens: 12,
            output_tokens: 4,
        }),
    );
    ledger
        .append_client_event(usage, 1, AppendDurability::Required)
        .unwrap();
    let runtime = event(
        &ledger,
        tree.run_id,
        None,
        None,
        ClientEventData::RuntimeError(RuntimeErrorPayload {
            error_code: "TRANSPORT_FAILURE".to_owned(),
            message: "connection ended".to_owned(),
        }),
    );
    ledger
        .append_client_event(runtime, 1, AppendDurability::Required)
        .unwrap();
    let minimal = ledger
        .events_after(0, EventProjection::Minimal, true)
        .unwrap();
    assert_eq!(minimal.len(), 1);
    assert_eq!(minimal[0].record.cursor, "2");
    assert!(minimal[0].replay);
    let operational = ledger
        .events_after(0, EventProjection::Operational, false)
        .unwrap();
    assert_eq!(operational.len(), 2);
    assert!(
        ledger
            .events_after(ledger.head().sequence + 1, EventProjection::Minimal, false)
            .is_err()
    );
    assert!(
        ledger
            .events_after(ledger.head().sequence, EventProjection::Minimal, false)
            .unwrap()
            .is_empty()
    );

    let invalid = event(
        &ledger,
        tree.run_id,
        None,
        None,
        ClientEventData::UsageReported(UsagePayload {
            input_tokens: 1,
            output_tokens: 1,
        }),
    );
    assert!(
        ledger
            .append_client_event(invalid, 1, AppendDurability::Required)
            .is_err()
    );
    assert_eq!(ledger.durable_records().len(), 2);
}

#[test]
fn client_events_reject_unrepresentable_unknown_and_schema_invalid_fields() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();

    let oversized = event(
        &ledger,
        tree.run_id,
        Some("thread-1"),
        Some("turn-1"),
        ClientEventData::WorkspaceChanges(WorkspaceChangesPayload {
            paths: vec![LosslessPath::Utf8("x".repeat(4096)); 600],
            truncated: false,
        }),
    );
    assert!(
        ledger
            .append_client_event(oversized, 1, AppendDurability::Required)
            .is_err()
    );

    let invalid_path = event(
        &ledger,
        tree.run_id,
        Some("thread-1"),
        Some("turn-1"),
        ClientEventData::WorkspaceChanges(WorkspaceChangesPayload {
            paths: vec![LosslessPath::Bytes {
                bytes: "not-base64".to_owned(),
            }],
            truncated: false,
        }),
    );
    assert!(
        ledger
            .append_client_event(invalid_path, 1, AppendDurability::Required)
            .is_err()
    );

    let invalid_artifact = event(
        &ledger,
        tree.run_id,
        Some("thread-1"),
        Some("turn-1"),
        ClientEventData::ResponseFinal(ResponseEventPayload {
            response: FinalResponse::Artifact {
                artifact: Box::new(ArtifactMetadata {
                    schema_version: 1,
                    artifact_id: Uuid::now_v7(),
                    run_id: tree.run_id,
                    kind: "final_response".to_owned(),
                    visibility: "observer".to_owned(),
                    interaction_request_id: None,
                    media_type: "text/markdown".to_owned(),
                    byte_length: 1,
                    sha256: "c".repeat(64),
                    created_at: "not-a-timestamp".to_owned(),
                    retention: "run_lifetime".to_owned(),
                    integrity: "verified".to_owned(),
                }),
            },
        }),
    );
    assert!(
        ledger
            .append_client_event(invalid_artifact, 1, AppendDurability::Required)
            .is_err()
    );

    let valid = event(
        &ledger,
        tree.run_id,
        Some("thread-1"),
        Some("turn-1"),
        ClientEventData::UsageReported(UsagePayload {
            input_tokens: 1,
            output_tokens: 1,
        }),
    );
    let mut value = serde_json::to_value(valid).unwrap();
    value["unexpected"] = serde_json::json!(true);
    let raw = serde_json::to_vec(&value).unwrap();
    let audit = AuditRecord::new_represented(
        ledger.next_sequence(),
        TIMESTAMP,
        tree.run_id,
        1,
        AuditKind::ClientEvent,
        &raw,
        ledger.previous_hash(),
    )
    .unwrap();
    assert!(ledger.append(audit, AppendDurability::Required).is_err());
    assert_eq!(ledger.durable_records().len(), 0);
}

#[test]
fn observer_delivery_is_bounded_and_resumable_by_sparse_cursor() {
    let tree = RunTree::new();
    let clock = TestClock::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, clock.clone(), NoFaults).unwrap();
    for _ in 0..=MAX_EVENT_DELIVERIES {
        let event = event(
            &ledger,
            tree.run_id,
            Some("thread-1"),
            Some("turn-1"),
            ClientEventData::UsageReported(UsagePayload {
                input_tokens: 1,
                output_tokens: 1,
            }),
        );
        ledger
            .append_client_event(event, 1, AppendDurability::Streaming)
            .unwrap();
    }
    ledger.flush().unwrap();
    let first = ledger
        .events_after(0, EventProjection::Operational, true)
        .unwrap();
    assert_eq!(first.len(), MAX_EVENT_DELIVERIES);
    let cursor = first.last().unwrap().record.cursor.parse::<u64>().unwrap();
    let second = ledger
        .events_after(cursor, EventProjection::Operational, true)
        .unwrap();
    assert_eq!(second.len(), 1);
}

#[test]
fn response_final_rejects_null_and_unavailable_schema_variants() {
    let tree = RunTree::new();
    let ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    let valid = event(
        &ledger,
        tree.run_id,
        Some("thread-1"),
        Some("turn-1"),
        ClientEventData::UsageReported(UsagePayload {
            input_tokens: 1,
            output_tokens: 1,
        }),
    );
    let mut null = serde_json::to_value(&valid).unwrap();
    null["type"] = serde_json::json!("response.final");
    null["payload"] = serde_json::json!({"response": null});
    assert!(serde_json::from_value::<ClientEventRecord>(null).is_err());

    let mut unavailable = serde_json::to_value(&valid).unwrap();
    unavailable["type"] = serde_json::json!("response.final");
    unavailable["payload"] = serde_json::json!({"response": {"kind": "unavailable"}});
    assert!(serde_json::from_value::<ClientEventRecord>(unavailable).is_err());
}

#[test]
fn reasoning_content_is_replaced_by_bounded_suppression_metadata() {
    let tree = RunTree::new();
    let mut ledger =
        Ledger::open_with(tree.root.clone(), tree.run_id, TestClock::new(), NoFaults).unwrap();
    let secret = br#"{"params":{"item":{"type":"reasoning","content":"PRIVATE CHAIN"}}}"#;
    let kind = ledger
        .append_app_server_message(
            AuditKind::AppServerNotification,
            "item/started",
            secret,
            1,
            AppendDurability::Required,
        )
        .unwrap();
    assert_eq!(kind, AuditKind::ReasoningContentSuppressed);
    let bytes = fs::read(tree.root.join("audit.jsonl")).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("PRIVATE CHAIN"));
    assert!(text.contains("reasoning_content_not_retained"));
    assert!(text.contains(&sha256_hex(secret)));

    let prefix_kind = ledger
        .append_app_server_message(
            AuditKind::AppServerNotification,
            "item/reasoning/delta",
            br#"{"delta":"SECRET"}"#,
            1,
            AppendDurability::Required,
        )
        .unwrap();
    assert_eq!(prefix_kind, AuditKind::ReasoningContentSuppressed);
    let ordinary_kind = ledger
        .append_app_server_message(
            AuditKind::AppServerNotification,
            "item/completed",
            br#"{"params":{"item":{"type":"agentMessage","text":"safe"}}}"#,
            1,
            AppendDurability::Required,
        )
        .unwrap();
    assert_eq!(ordinary_kind, AuditKind::AppServerNotification);
    assert!(
        ledger
            .append_app_server_message(
                AuditKind::RunCreated,
                "item/completed",
                b"{}",
                1,
                AppendDurability::Required,
            )
            .is_err()
    );
    assert!(
        ledger
            .append_app_server_message(
                AuditKind::AppServerNotification,
                &"x".repeat(257),
                b"{}",
                1,
                AppendDurability::Required,
            )
            .is_err()
    );
}

fn next_record<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    ledger: &Ledger<C, F>,
    kind: AuditKind,
    payload: LosslessJson,
) -> AuditRecord {
    AuditRecord::new(
        ledger.next_sequence(),
        TIMESTAMP,
        fixed_run_id(),
        1,
        kind,
        payload,
        ledger.previous_hash(),
    )
    .unwrap()
}

fn event<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    ledger: &Ledger<C, F>,
    run_id: Uuid,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
    data: ClientEventData,
) -> ClientEventRecord {
    ClientEventRecord {
        schema_version: 1,
        event_schema_version: 1,
        cursor: ledger.next_sequence().to_string(),
        event_id: Uuid::now_v7(),
        timestamp: TIMESTAMP.to_owned(),
        workspace_id: "a".repeat(64),
        run_id,
        thread_id: thread_id.map(str::to_owned),
        turn_id: turn_id.map(str::to_owned),
        server_key: "b".repeat(64),
        server_epoch: 1,
        data,
    }
}

fn object(entries: &[(&str, LosslessJson)]) -> LosslessJson {
    LosslessJson::Object(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
    )
}

fn string(value: &str) -> LosslessJson {
    LosslessJson::String(value.to_owned())
}

fn fixed_run_id() -> Uuid {
    Uuid::parse_str("018f0c6a-7b01-7abc-8def-0123456789ab").unwrap()
}

fn read_state(root: &Path) -> RunStateProjection {
    serde_json::from_slice(&fs::read(root.join("state.json")).unwrap()).unwrap()
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "condition did not become true in time"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn write_state(root: &Path, state: &RunStateProjection) {
    fs::write(root.join("state.json"), serde_json::to_vec(state).unwrap()).unwrap();
    fs::set_permissions(root.join("state.json"), fs::Permissions::from_mode(0o600)).unwrap();
}

struct RunTree {
    root: PathBuf,
    run_id: Uuid,
}

impl RunTree {
    fn new() -> Self {
        let run_id = fixed_run_id();
        let root = std::env::temp_dir().join(format!("dolgorae-ledger-test-{}", Uuid::now_v7()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("recovery"))
            .unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        Self { root, run_id }
    }
}

impl Drop for RunTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
