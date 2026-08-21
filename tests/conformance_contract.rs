use dolgorae::audit::{AUDIT_KINDS, AuditKind, AuditRecord};
use dolgorae::conformance::{
    BootstrapRecordKind, BootstrapRequest, ConformanceError, ConformantLedger, IdempotencyIntent,
    IdempotencyOperation, IdempotencyReservations, ReservationResult, delete_run_confirmed,
    lifecycle_transition_allowed, verify_ledger_records,
};
use dolgorae::domain::RunLifecycle;
use dolgorae::fault::NoFaults;
use dolgorae::jcs::{LosslessJson, canonicalize, parse};
use dolgorae::ledger::{AppendDurability, Ledger, LedgerClock, LedgerError};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

const TIMESTAMP: &str = "2026-08-21T12:34:56.123456Z";

#[derive(Default)]
struct TestClock(AtomicU64);

impl LedgerClock for TestClock {
    fn monotonic_millis(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }

    fn timestamp(&self) -> String {
        TIMESTAMP.to_owned()
    }
}

struct RunTree {
    parent: PathBuf,
    root: PathBuf,
    run_id: Uuid,
}

impl RunTree {
    fn new() -> Self {
        let run_id = Uuid::now_v7();
        let parent = std::env::temp_dir().join(format!("dolgorae-conformance-{}", Uuid::now_v7()));
        let runs = parent.join("runs");
        fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
        fs::DirBuilder::new().mode(0o700).create(&runs).unwrap();
        let root = runs.join(run_id.to_string());
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
        Self {
            parent,
            root,
            run_id,
        }
    }

    fn ledger(&self) -> ConformantLedger<TestClock, NoFaults> {
        ConformantLedger::open_with(&self.root, self.run_id, TestClock::default(), NoFaults)
            .unwrap()
    }
}

impl Drop for RunTree {
    fn drop(&mut self) {
        if self.parent.exists() {
            fs::remove_dir_all(&self.parent).unwrap();
        }
    }
}

fn intent(run_id: Uuid, key: &str, digest: char) -> IdempotencyIntent {
    IdempotencyIntent {
        schema_version: 1,
        operation: IdempotencyOperation::StartRun,
        idempotency_key: key.to_owned(),
        normalized_identity_sha256: digest.to_string().repeat(64),
        run_id,
    }
}

fn bootstrap(ledger: &mut ConformantLedger<TestClock, NoFaults>, run_id: Uuid) {
    ledger
        .bootstrap(&BootstrapRequest {
            timestamp: TIMESTAMP.to_owned(),
            workspace_id: "a".repeat(64),
            intent: intent(run_id, "allocate", 'b'),
            record_kind: BootstrapRecordKind::RunCreated,
            initial_access: "read".to_owned(),
            default_effort: Some("medium".to_owned()),
        })
        .unwrap();
}

fn payload(input: &str) -> LosslessJson {
    parse(input).unwrap()
}

fn next_record(
    ledger: &ConformantLedger<TestClock, NoFaults>,
    kind: AuditKind,
    payload: LosslessJson,
) -> AuditRecord {
    AuditRecord::new(
        ledger.inner().next_sequence(),
        TIMESTAMP,
        ledger.inner().projection().run_id,
        ledger.inner().projection().run_generation,
        kind,
        payload,
        ledger.inner().previous_hash(),
    )
    .unwrap()
}

fn transition(
    ledger: &ConformantLedger<TestClock, NoFaults>,
    previous: &str,
    current: &str,
) -> AuditRecord {
    next_record(
        ledger,
        AuditKind::LifecycleTransition,
        payload(&format!(
            r#"{{"previous":"{previous}","current":"{current}","terminal_seal":false}}"#
        )),
    )
}

#[test]
fn virgin_and_allocated_ledgers_reconstruct_without_a_worker() {
    let tree = RunTree::new();
    let mut ledger = tree.ledger();
    let virgin = ledger.verify().unwrap();
    assert_eq!(virgin.record_count, 0);
    assert_eq!(virgin.bootstrap_kind, None);

    bootstrap(&mut ledger, tree.run_id);
    let allocated = ledger.verify().unwrap();
    assert_eq!(allocated.record_count, 3);
    assert_eq!(allocated.lifecycle, RunLifecycle::Starting);
    assert_eq!(
        allocated.bootstrap_kind,
        Some(BootstrapRecordKind::RunCreated)
    );
    assert_eq!(
        allocated.idempotency_intent.unwrap().idempotency_key,
        "allocate"
    );
    drop(ledger);

    let reopened = tree.ledger();
    assert_eq!(reopened.verify().unwrap().record_count, 3);
    assert_eq!(
        reopened.inner().projection().default_effort.as_deref(),
        Some("medium")
    );
}

#[test]
fn bootstrap_resumes_each_exact_durable_prefix() {
    let tree = RunTree::new();
    let request = BootstrapRequest {
        timestamp: TIMESTAMP.to_owned(),
        workspace_id: "a".repeat(64),
        intent: intent(tree.run_id, "allocate", 'b'),
        record_kind: BootstrapRecordKind::RunCreated,
        initial_access: "read".to_owned(),
        default_effort: Some("medium".to_owned()),
    };
    let workspace = AuditRecord::new(
        1,
        TIMESTAMP,
        tree.run_id,
        0,
        AuditKind::WorkspaceInitialized,
        payload(&format!(r#"{{"workspace_id":"{}"}}"#, "a".repeat(64))),
        "0".repeat(64),
    )
    .unwrap();
    let reserved = AuditRecord::new(
        2,
        TIMESTAMP,
        tree.run_id,
        0,
        AuditKind::IdempotencyReserved,
        payload(&serde_json::to_string(&request.intent).unwrap()),
        workspace.hash(),
    )
    .unwrap();
    let mut prefix = workspace.canonical_line().unwrap();
    prefix.extend(reserved.canonical_line().unwrap());
    fs::write(tree.root.join("audit.jsonl"), prefix).unwrap();

    let mut recovered = ConformantLedger::open_for_bootstrap_with(
        &tree.root,
        tree.run_id,
        TestClock::default(),
        NoFaults,
    )
    .unwrap();
    assert_eq!(recovered.bootstrap(&request).unwrap().record_count, 3);
    assert_eq!(tree.ledger().verify().unwrap().record_count, 3);
}

#[test]
fn bootstrap_single_record_prefix_allows_only_exact_completion() {
    let tree = RunTree::new();
    let request = BootstrapRequest {
        timestamp: TIMESTAMP.to_owned(),
        workspace_id: "a".repeat(64),
        intent: intent(tree.run_id, "allocate", 'b'),
        record_kind: BootstrapRecordKind::RunCreated,
        initial_access: "read".to_owned(),
        default_effort: Some("medium".to_owned()),
    };
    let workspace = AuditRecord::new(
        1,
        TIMESTAMP,
        tree.run_id,
        0,
        AuditKind::WorkspaceInitialized,
        payload(&format!(r#"{{"workspace_id":"{}"}}"#, "a".repeat(64))),
        "0".repeat(64),
    )
    .unwrap();
    fs::write(
        tree.root.join("audit.jsonl"),
        workspace.canonical_line().unwrap(),
    )
    .unwrap();
    let mut recovered = ConformantLedger::open_for_bootstrap_with(
        &tree.root,
        tree.run_id,
        TestClock::default(),
        NoFaults,
    )
    .unwrap();
    let bypass = AuditRecord::new(
        2,
        TIMESTAMP,
        tree.run_id,
        0,
        AuditKind::ProfileObserved,
        payload("{}"),
        workspace.hash(),
    )
    .unwrap();
    assert!(matches!(
        recovered.append(bypass, AppendDurability::Required),
        Err(ConformanceError::InvalidHistory(_))
    ));
    assert_eq!(recovered.bootstrap(&request).unwrap().record_count, 3);
}

#[test]
fn unaccepted_reservation_releases_and_accepted_identity_replays_exactly() {
    let tree = RunTree::new();
    let run_id = tree.run_id;
    let reservations = IdempotencyReservations::default();
    let first = intent(run_id, "same-key", 'a');
    let ReservationResult::Acquired(guard) = reservations.reserve(&first).unwrap() else {
        panic!("first reservation must be acquired");
    };
    assert!(matches!(
        reservations.reserve(&first),
        Err(ConformanceError::ReservationPending)
    ));
    drop(guard);
    assert!(!reservations.contains(IdempotencyOperation::StartRun, "same-key"));

    let ReservationResult::Acquired(guard) = reservations.reserve(&first).unwrap() else {
        panic!("released reservation must be acquirable");
    };
    let mut ledger = tree.ledger();
    let report = ledger
        .bootstrap(&BootstrapRequest {
            timestamp: TIMESTAMP.to_owned(),
            workspace_id: "a".repeat(64),
            intent: first.clone(),
            record_kind: BootstrapRecordKind::RunCreated,
            initial_access: "read".to_owned(),
            default_effort: None,
        })
        .unwrap();
    assert_eq!(guard.accept_after_publication(&report).unwrap(), run_id);
    assert!(matches!(
        reservations.reserve(&first).unwrap(),
        ReservationResult::ExactReplay(replayed) if replayed == run_id
    ));
    let conflict = intent(Uuid::now_v7(), "same-key", 'c');
    assert!(matches!(
        reservations.reserve(&conflict),
        Err(ConformanceError::ReservationConflict)
    ));
    let other_operation = IdempotencyIntent {
        operation: IdempotencyOperation::SubmitTurn,
        ..conflict
    };
    assert!(matches!(
        reservations.reserve(&other_operation),
        Ok(ReservationResult::Acquired(_))
    ));
}

#[test]
fn utf8_bounds_and_durable_idempotency_conflicts_fail_closed() {
    let tree = RunTree::new();
    let oversized = intent(tree.run_id, &"가".repeat(86), 'a');
    assert!(matches!(
        oversized.validate(),
        Err(ConformanceError::InvalidIntent(_))
    ));

    let mut ledger = tree.ledger();
    bootstrap(&mut ledger, tree.run_id);
    let conflict = IdempotencyIntent {
        normalized_identity_sha256: "c".repeat(64),
        ..intent(tree.run_id, "allocate", 'b')
    };
    let record = next_record(
        &ledger,
        AuditKind::IdempotencyReserved,
        payload(&serde_json::to_string(&conflict).unwrap()),
    );
    assert!(matches!(
        ledger.append(record, AppendDurability::Required),
        Err(ConformanceError::InvalidHistory(_))
    ));
}

#[test]
fn implicit_edges_and_dangling_terminal_evidence_fail_closed() {
    let tree = RunTree::new();
    let mut ledger = tree.ledger();
    bootstrap(&mut ledger, tree.run_id);
    let illegal_turn = next_record(
        &ledger,
        AuditKind::TurnStarted,
        payload(r#"{"turn_id":"turn-1"}"#),
    );
    assert!(matches!(
        ledger.append(illegal_turn, AppendDurability::Required),
        Err(ConformanceError::InvalidHistory(_))
    ));

    let cleanup = next_record(
        &ledger,
        AuditKind::CleanupResult,
        payload(r#"{"outcome":"worker absent"}"#),
    );
    let mut dangling = ledger.inner().durable_records().to_vec();
    dangling.push(cleanup);
    assert!(matches!(
        verify_ledger_records(tree.run_id, &dangling),
        Err(ConformanceError::InvalidHistory(_))
    ));
}

#[test]
fn raw_ledger_cannot_bypass_lifecycle_authority() {
    let tree = RunTree::new();
    let mut raw =
        Ledger::open_with(&tree.root, tree.run_id, TestClock::default(), NoFaults).unwrap();
    let evidence = AuditRecord::new(
        1,
        TIMESTAMP,
        tree.run_id,
        0,
        AuditKind::StartFailed,
        payload(r#"{"reason":"forged"}"#),
        "0".repeat(64),
    )
    .unwrap();
    assert!(matches!(
        raw.append(evidence, AppendDurability::Required),
        Err(LedgerError::InvalidRecord(_))
    ));
}

#[test]
fn start_failed_requires_exact_bootstrap_authority_and_is_terminal() {
    let tree = RunTree::new();
    let mut ledger = tree.ledger();
    bootstrap(&mut ledger, tree.run_id);
    let evidence = next_record(
        &ledger,
        AuditKind::StartFailed,
        payload(r#"{"reason":"spawn failed"}"#),
    );
    assert!(matches!(
        ledger.append(evidence.clone(), AppendDurability::Required),
        Err(ConformanceError::InvalidHistory(_))
    ));
    let seal = AuditRecord::new(
        evidence.sequence() + 1,
        TIMESTAMP,
        tree.run_id,
        0,
        AuditKind::LifecycleTransition,
        payload(r#"{"previous":"starting","current":"start_failed","terminal_seal":true}"#),
        evidence.hash(),
    )
    .unwrap();
    let mut history = ledger.inner().durable_records().to_vec();
    history.extend([evidence, seal]);
    let report = verify_ledger_records(tree.run_id, &history).unwrap();
    assert_eq!(report.lifecycle, RunLifecycle::StartFailed);
    assert!(report.terminal_sealed);
}

#[test]
fn closed_run_requires_cleanup_evidence_and_reconstructs_its_seal() {
    let tree = RunTree::new();
    let mut ledger = tree.ledger();
    bootstrap(&mut ledger, tree.run_id);
    let idle = transition(&ledger, "starting", "idle");
    ledger.append(idle, AppendDurability::Required).unwrap();
    let report = ledger.seal_closed(TIMESTAMP, "worker absent").unwrap();
    assert_eq!(report.lifecycle, RunLifecycle::Closed);
    assert!(report.terminal_sealed);
    drop(ledger);
    assert_eq!(
        tree.ledger().verify().unwrap().lifecycle,
        RunLifecycle::Closed
    );
}

#[test]
fn invalid_terminal_or_nonterminal_seals_are_refused_before_write() {
    let tree = RunTree::new();
    let mut ledger = tree.ledger();
    bootstrap(&mut ledger, tree.run_id);
    let missing_evidence = next_record(
        &ledger,
        AuditKind::LifecycleTransition,
        payload(r#"{"previous":"starting","current":"start_failed","terminal_seal":true}"#),
    );
    assert!(matches!(
        ledger.append(missing_evidence, AppendDurability::Required),
        Err(ConformanceError::InvalidHistory(_))
    ));
    let false_seal = next_record(
        &ledger,
        AuditKind::LifecycleTransition,
        payload(r#"{"previous":"starting","current":"idle","terminal_seal":true}"#),
    );
    assert!(matches!(
        ledger.append(false_seal, AppendDurability::Required),
        Err(ConformanceError::InvalidHistory(_))
    ));
    assert_eq!(ledger.inner().durable_records().len(), 3);
}

#[test]
fn every_declared_lifecycle_edge_is_closed_and_exact() {
    let states = [
        RunLifecycle::Starting,
        RunLifecycle::Idle,
        RunLifecycle::Running,
        RunLifecycle::WaitingInteraction,
        RunLifecycle::ReconciliationRequired,
        RunLifecycle::Paused,
        RunLifecycle::Closed,
        RunLifecycle::StartFailed,
        RunLifecycle::OutcomeUnknown,
    ];
    let allowed = [
        (RunLifecycle::Starting, RunLifecycle::Idle),
        (RunLifecycle::Starting, RunLifecycle::StartFailed),
        (RunLifecycle::Idle, RunLifecycle::Running),
        (RunLifecycle::Idle, RunLifecycle::Paused),
        (RunLifecycle::Idle, RunLifecycle::Closed),
        (RunLifecycle::Running, RunLifecycle::Idle),
        (RunLifecycle::Running, RunLifecycle::WaitingInteraction),
        (RunLifecycle::Running, RunLifecycle::ReconciliationRequired),
        (RunLifecycle::Running, RunLifecycle::OutcomeUnknown),
        (RunLifecycle::Running, RunLifecycle::Paused),
        (RunLifecycle::Running, RunLifecycle::Closed),
        (RunLifecycle::WaitingInteraction, RunLifecycle::Running),
        (RunLifecycle::WaitingInteraction, RunLifecycle::Idle),
        (
            RunLifecycle::WaitingInteraction,
            RunLifecycle::ReconciliationRequired,
        ),
        (
            RunLifecycle::WaitingInteraction,
            RunLifecycle::OutcomeUnknown,
        ),
        (RunLifecycle::WaitingInteraction, RunLifecycle::Paused),
        (RunLifecycle::WaitingInteraction, RunLifecycle::Closed),
        (RunLifecycle::ReconciliationRequired, RunLifecycle::Paused),
        (
            RunLifecycle::ReconciliationRequired,
            RunLifecycle::OutcomeUnknown,
        ),
        (RunLifecycle::Paused, RunLifecycle::Idle),
        (RunLifecycle::Paused, RunLifecycle::Closed),
        (
            RunLifecycle::OutcomeUnknown,
            RunLifecycle::ReconciliationRequired,
        ),
        (RunLifecycle::OutcomeUnknown, RunLifecycle::Paused),
        (RunLifecycle::OutcomeUnknown, RunLifecycle::Closed),
    ];
    for previous in states {
        for current in states {
            assert_eq!(
                lifecycle_transition_allowed(previous, current),
                allowed.contains(&(previous, current)),
                "unexpected edge {} -> {}",
                previous.as_str(),
                current.as_str()
            );
        }
    }
}

#[test]
fn checked_conformance_fixture_matches_the_rust_closed_sets() {
    let fixture: serde_json::Value = serde_json::from_slice(
        &fs::read("docs/protocol/dolgorae-ledger-conformance-v1.json").unwrap(),
    )
    .unwrap();
    let fixture_kinds: Vec<&str> = fixture["record_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    let rust_kinds: Vec<&str> = AUDIT_KINDS.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(fixture_kinds, rust_kinds);

    let fixture_edges: Vec<(&str, &str)> = fixture["lifecycle_transitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| {
            let edge = edge.as_array().unwrap();
            (edge[0].as_str().unwrap(), edge[1].as_str().unwrap())
        })
        .collect();
    for previous in [
        RunLifecycle::Starting,
        RunLifecycle::Idle,
        RunLifecycle::Running,
        RunLifecycle::WaitingInteraction,
        RunLifecycle::ReconciliationRequired,
        RunLifecycle::Paused,
        RunLifecycle::Closed,
        RunLifecycle::StartFailed,
        RunLifecycle::OutcomeUnknown,
    ] {
        for current in [
            RunLifecycle::Starting,
            RunLifecycle::Idle,
            RunLifecycle::Running,
            RunLifecycle::WaitingInteraction,
            RunLifecycle::ReconciliationRequired,
            RunLifecycle::Paused,
            RunLifecycle::Closed,
            RunLifecycle::StartFailed,
            RunLifecycle::OutcomeUnknown,
        ] {
            assert_eq!(
                lifecycle_transition_allowed(previous, current),
                fixture_edges.contains(&(previous.as_str(), current.as_str()))
            );
        }
    }
}

#[test]
fn integrity_failure_blocks_mutation_but_confirmed_terminal_delete_escapes() {
    let tree = RunTree::new();
    let mut ledger = tree.ledger();
    bootstrap(&mut ledger, tree.run_id);
    let idle = transition(&ledger, "starting", "idle");
    ledger.append(idle, AppendDurability::Required).unwrap();
    ledger.seal_closed(TIMESTAMP, "worker absent").unwrap();
    drop(ledger);

    let audit = tree.root.join("audit.jsonl");
    let mut bytes = fs::read(&audit).unwrap();
    let position = bytes.iter().position(|byte| *byte == b'a').unwrap();
    bytes[position] = b'b';
    fs::write(&audit, bytes).unwrap();
    assert!(matches!(
        ConformantLedger::open(&tree.root, tree.run_id),
        Err(ConformanceError::Ledger(_))
    ));
    assert!(matches!(
        delete_run_confirmed(&tree.parent, tree.run_id, false),
        Err(ConformanceError::ConfirmationRequired)
    ));
    delete_run_confirmed(&tree.parent, tree.run_id, true).unwrap();
    assert!(!tree.root.exists());
}

#[test]
fn delete_escape_rejects_an_incomplete_terminal_seal_payload() {
    let tree = RunTree::new();
    let mut ledger = tree.ledger();
    bootstrap(&mut ledger, tree.run_id);
    let idle = transition(&ledger, "starting", "idle");
    ledger.append(idle, AppendDurability::Required).unwrap();
    ledger.seal_closed(TIMESTAMP, "worker absent").unwrap();

    let records = ledger.inner().durable_records().to_vec();
    let previous = &records[records.len() - 2];
    let final_record = &records[records.len() - 1];
    let malformed = AuditRecord::new(
        final_record.sequence(),
        TIMESTAMP,
        tree.run_id,
        final_record.run_generation(),
        AuditKind::LifecycleTransition,
        payload(r#"{"current":"closed","terminal_seal":true}"#),
        previous.hash(),
    )
    .unwrap();
    let mut audit = Vec::new();
    for record in &records[..records.len() - 1] {
        audit.extend(record.canonical_line().unwrap());
    }
    audit.extend(malformed.canonical_line().unwrap());
    fs::write(tree.root.join("audit.jsonl"), audit).unwrap();

    let mut state = ledger.inner().projection();
    state.ledger_head.sequence = malformed.sequence();
    state.ledger_head.hash = malformed.hash().to_owned();
    let state_json = serde_json::to_string(&state).unwrap();
    let mut state_bytes = canonicalize(&parse(&state_json).unwrap()).unwrap();
    state_bytes.push(b'\n');
    fs::write(tree.root.join("state.json"), state_bytes).unwrap();
    drop(ledger);

    assert!(delete_run_confirmed(&tree.parent, tree.run_id, true).is_err());
    assert!(tree.root.exists());
}

#[test]
fn write_continuation_uses_the_only_alternate_bootstrap_kind() {
    let tree = RunTree::new();
    let mut ledger = tree.ledger();
    let request = BootstrapRequest {
        timestamp: TIMESTAMP.to_owned(),
        workspace_id: "d".repeat(64),
        intent: IdempotencyIntent {
            operation: IdempotencyOperation::CreateWriteContinuation,
            ..intent(tree.run_id, "continue", 'e')
        },
        record_kind: BootstrapRecordKind::WriteContinuationCreated,
        initial_access: "read".to_owned(),
        default_effort: None,
    };
    assert_eq!(
        ledger.bootstrap(&request).unwrap().bootstrap_kind,
        Some(BootstrapRecordKind::WriteContinuationCreated)
    );
}
