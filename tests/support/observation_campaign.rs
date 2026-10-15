//! Test-owned fixture preparation and measurement accounting; no real-account use.

use super::fixture_support::Fixture;
use dolgorae::darwin::DarwinSystem;
use dolgorae::domain::{Assurance, ExecutionLane, Purpose, PurposeKind};
use dolgorae::event::{ClientEventData, FinalResponse, ResponseEventPayload};
use dolgorae::ledger::{AppendDurability, Ledger, LedgerClock as _, SystemLedgerClock};
use dolgorae::machine::MachineError;
use dolgorae::orchestration::*;
use dolgorae::run::{AgentConfigurationSnapshot, InstructionSnapshot};
use dolgorae::specialist_policy::*;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub fn sha256(path: &Path) -> String {
    use std::io::Read as _;
    let mut input = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let size = input.read(&mut chunk).unwrap();
        if size == 0 {
            break;
        }
        hash.update(&chunk[..size]);
    }
    data_encoding::HEXLOWER.encode(&hash.finalize())
}

pub fn assert_turn_history(state_root: &Path, id: &str, expected: usize) {
    let run_id = Uuid::parse_str(id).unwrap();
    let (snapshot, _) = dolgorae::snapshot::RunSnapshot::observe(state_root, run_id, 0).unwrap();
    let observed = dolgorae::ledger::ObservedLedger::open_complete(
        &state_root.join("runs").join(id),
        run_id,
        snapshot.stamp.run_state_revision,
    )
    .unwrap();
    let mut canonical = Vec::new();
    observed.write_canonical_history(&mut canonical).unwrap();
    let records = canonical
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| dolgorae::audit::AuditRecord::from_canonical_line(line).unwrap())
        .collect::<Vec<_>>();
    let turns = records
        .iter()
        .filter(|record| record.kind().as_str() == "turn_started")
        .count();
    let terminals = records
        .iter()
        .filter(|record| record.kind().as_str() == "turn_terminal")
        .count();
    assert_eq!(turns, expected);
    assert_eq!(terminals, expected);
}

pub fn native_turn_dispatches(root: &Path) -> usize {
    fs::read_to_string(root.join("transcript.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|message| message["method"] == "turn/start")
        .count()
}

pub fn augment_timeline_and_restore_worker(fixture: &Fixture, id: &str, target: usize) {
    let run_id = Uuid::parse_str(id).unwrap();
    let uid = DarwinSystem.current_uid();
    let runtime = dolgorae::worker::runtime_record_path(
        &dolgorae::worker::runtime_root(&fixture.state_root),
        run_id,
    )
    .unwrap();
    let previous = dolgorae::worker::read_runtime_record(&runtime, uid).unwrap();
    let response =
        dolgorae::worker::call_run_worker(&fixture.state_root, run_id, uid, None, |expected| {
            dolgorae::worker::ControlRequestV1::Shutdown { expected }
        })
        .unwrap();
    assert!(matches!(
        response,
        dolgorae::worker::ControlResponseV1::Shutdown {
            terminal_confirmed: true,
            ..
        }
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let same_process = DarwinSystem
            .bsd_process_identity(previous.identity.pid)
            .is_ok_and(|actual| {
                actual.start_tvsec == previous.identity.start_tvsec
                    && actual.start_tvusec == previous.identity.start_tvusec
                    && !actual.zombie
            });
        // Frozen Shutdown can retain its locator for canonical recovery. The
        // recorded lifetime must be absent; Ledger::open then acquires the
        // actual writer lock before any synthetic fixture append.
        if !same_process {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fixture worker did not exit cleanly before synthetic history preparation"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let turn_dispatches = || native_turn_dispatches(&fixture.root);
    let dispatches_before_augmentation = turn_dispatches();
    assert!(dispatches_before_augmentation >= 30);
    let ledger_root = fixture.state_root.join("runs").join(id);
    let mut ledger = Ledger::open(&ledger_root, run_id).unwrap();
    let records = ledger.durable_records().unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|record| record.kind().as_str() == "turn_started")
            .count(),
        30
    );
    let mut template = records
        .iter()
        .rev()
        .find_map(|record| {
            record
                .client_projection()
                .map(|projection| projection.record.clone())
        })
        .unwrap();
    let count = records
        .iter()
        .filter(|record| {
            record.kind().as_str() == "turn_started"
                || record.kind().as_str() == "turn_terminal"
                || record.client_projection().is_some_and(|projection| {
                    matches!(projection.record.data, ClientEventData::ResponseFinal(_))
                })
        })
        .count();
    assert_eq!(
        count, 90,
        "30 completed fixture Turns must yield 90 original timeline items"
    );
    assert_eq!(target, 500);
    template.turn_id = Some("turn-30".to_owned());
    let context = dolgorae::event::EventAppendContext {
        workspace_id: template.workspace_id.clone(),
        server_key: template.server_key.clone(),
        server_epoch: template.server_epoch,
        writer_state_revision: records
            .iter()
            .rev()
            .find_map(|record| {
                record
                    .client_projection()
                    .map(|projection| projection.stamp.writer_state_revision)
            })
            .unwrap(),
    };
    for index in count..target {
        let mut record = template.clone();
        record.cursor = ledger.next_sequence().to_string();
        record.event_id = Uuid::now_v7();
        record.timestamp = SystemLedgerClock::default().timestamp();
        record.data = ClientEventData::ResponseFinal(ResponseEventPayload {
            response: FinalResponse::Inline {
                text: format!("Synthetic timeline fixture item {index}; no extra provider Turn."),
            },
        });
        ledger
            .with_event_context(context.clone(), |ledger| {
                ledger.append_client_event(
                    record,
                    previous.identity.run_generation,
                    AppendDurability::Required,
                )
            })
            .unwrap();
    }
    ledger.flush().unwrap();
    drop(ledger);
    // Existing canonical recovery establishes fresh owner, history and writer
    // proof. It must not replay any of the 30 accepted Turns.
    fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "recover",
        id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    let state = fixture.cli(&[
        "run",
        "status",
        id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert_eq!(state["state"], "idle");
    verify_healthy_worker(&fixture.state_root, run_id);
    assert_turn_history(&fixture.state_root, id, 30);
    assert_eq!(
        turn_dispatches(),
        dispatches_before_augmentation,
        "recovery replayed an accepted Turn"
    );
    let timeline = fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "timeline",
        id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--limit",
        "500",
    ]);
    assert_eq!(timeline["items"].as_array().unwrap().len(), target);
    assert!(timeline["next_after_cursor"].is_null());
}

pub fn verify_healthy_worker(state_root: &Path, run_id: Uuid) {
    let uid = DarwinSystem.current_uid();
    let path =
        dolgorae::worker::runtime_record_path(&dolgorae::worker::runtime_root(state_root), run_id)
            .unwrap();
    let record = dolgorae::worker::read_runtime_record(&path, uid).unwrap();
    assert!(!record.control_recovery_required);
    let process = DarwinSystem
        .bsd_process_identity(record.identity.pid)
        .unwrap();
    assert!(!process.zombie);
    assert_eq!(process.uid, record.identity.uid);
    assert_eq!(
        (process.start_tvsec, process.start_tvusec),
        (record.identity.start_tvsec, record.identity.start_tvusec)
    );
    assert_eq!(
        DarwinSystem.live_process_path(record.identity.pid).unwrap(),
        record.identity.executable_path
    );
    let response = dolgorae::worker::call_run_worker(state_root, run_id, uid, None, |expected| {
        dolgorae::worker::ControlRequestV1::Hello { expected }
    })
    .unwrap();
    assert!(matches!(
        response,
        dolgorae::worker::ControlResponseV1::Hello { .. }
    ));
}

pub fn runtime_configuration() -> Value {
    let output = |program: &str, args: &[&str]| {
        let result = Command::new(program).args(args).output().unwrap();
        assert!(result.status.success());
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    };
    json!({"platform":output("/usr/bin/sw_vers",&["-productVersion"]),"architecture":std::env::consts::ARCH,
        "cpu":output("/usr/sbin/sysctl",&["-n","machdep.cpu.brand_string"]),"memory_bytes":output("/usr/sbin/sysctl",&["-n","hw.memsize"]),
        "rust":output("rustc",&["--version"]),"buf":output("buf",&["--version"]),"go":output("go",&["version"]),
        "python":output(concat!(env!("CARGO_MANIFEST_DIR"),"/.venv/bin/python"),&["--version"]),
        "package_version":env!("CARGO_PKG_VERSION"),"cpu_measurement":"ps cumulative CPU seconds of instrumented gateway plus fixture-owned native executables; process census sampled at interval boundaries","clock":"std::time::Instant monotonic; sample units ns"})
}

#[derive(Clone)]
struct TaskAdapter {
    finished: Arc<AtomicBool>,
    observations: Arc<AtomicU64>,
    target: Option<Uuid>,
    completion_created_at: Arc<OnceLock<String>>,
}
impl OrchestrationAdapter for TaskAdapter {
    fn resolve_task_contexts(
        &mut self,
        _source_run_id: Uuid,
        references: &[Uuid],
    ) -> Result<Vec<AcceptedTaskContext>, MachineError> {
        assert!(references.is_empty());
        Ok(Vec::new())
    }
    fn publish_approval_request(
        &mut self,
        _session_id: Uuid,
        _operation_id: Uuid,
        _approval_request_id: Uuid,
        _request: &RequestSpecialist,
    ) -> Result<(), AdapterFailure> {
        Ok(())
    }
    fn publish_specialist(
        &mut self,
        _plan: &BrokeredRunPlan,
        _credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure> {
        Ok(())
    }
    fn create_thread(
        &mut self,
        _plan: &BrokeredRunPlan,
        _credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure> {
        Ok(())
    }
    fn observe_specialist(
        &mut self,
        _plan: &BrokeredRunPlan,
        _credential: &BrokerCredential,
    ) -> Result<SpecialistPublicationObservation, AdapterFailure> {
        Ok(SpecialistPublicationObservation::Ready)
    }
    fn dispatch_task(
        &mut self,
        _member: &BrokeredMemberSnapshot,
        task: &AcceptedSpecialistTask,
        _credential: &BrokerCredential,
    ) -> Result<TaskDispatch, AdapterFailure> {
        Ok(TaskDispatch::Accepted {
            turn_id: format!("component-task-{}", task.task_id),
        })
    }
    fn observe_task(
        &mut self,
        task: &SpecialistTaskSnapshot,
        _credential: &BrokerCredential,
    ) -> Result<SpecialistTaskObservation, AdapterFailure> {
        self.observations.fetch_add(1, Ordering::Relaxed);
        if self.finished.load(Ordering::Acquire) && self.target == Some(task.task_id) {
            let value = json!({"answer":"Scripted independent component completion."});
            Ok(SpecialistTaskObservation::Terminal {
                status: "completed".to_owned(),
                output: Some(CompletedTaskOutput {
                    bytes: serde_json::to_vec(&value).unwrap(),
                    value,
                    created_at: self
                        .completion_created_at
                        .get()
                        .expect("scripted completion timestamp published before ready")
                        .clone(),
                }),
            })
        } else {
            Ok(SpecialistTaskObservation::Running)
        }
    }
    fn cancel_task(
        &mut self,
        _task: &SpecialistTaskSnapshot,
        _credential: &BrokerCredential,
    ) -> Result<TaskCancellation, AdapterFailure> {
        Ok(TaskCancellation::TerminalInterrupted)
    }
    fn release_specialist(
        &mut self,
        _member: &BrokeredMemberSnapshot,
        _credential: &BrokerCredential,
    ) -> Result<(), AdapterFailure> {
        Ok(())
    }
    fn release_writer(&mut self, _run_id: Uuid) -> Result<(), AdapterFailure> {
        Ok(())
    }
    fn verify_writer_none(&mut self) -> Result<(), AdapterFailure> {
        Ok(())
    }
    fn acquire_writer(&mut self, _run_id: Uuid) -> Result<(), AdapterFailure> {
        Ok(())
    }
}

fn call_context(session: Uuid, key: &str) -> PrimaryCallContext {
    PrimaryCallContext {
        session_id: session,
        source_run_id: session,
        source_turn_id: "component-primary".to_owned(),
        source_tool_call_id: format!("component-call-{key}"),
        idempotency_key: key.to_owned(),
    }
}

fn await_payload(tasks: &[Uuid], seconds: u64) -> Value {
    json!({"operation":"await_specialist_tasks","task_ids":tasks,"return_when":"any","transport_wait_seconds":seconds})
}

fn cumulative_cpu_seconds(root: &Path) -> (f64, usize) {
    let mut pids = vec![std::process::id()];
    for process in DarwinSystem.all_process_identities().unwrap() {
        if process.pid != std::process::id()
            && DarwinSystem
                .live_process_path(process.pid)
                .is_some_and(|path| path.starts_with(root))
        {
            pids.push(process.pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    let selection = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new("/bin/ps")
        .args(["-p", &selection, "-o", "time="])
        .output()
        .unwrap();
    assert!(output.status.success());
    let total = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let parts = line
                .trim()
                .split(':')
                .map(|part| part.parse::<f64>().unwrap())
                .collect::<Vec<_>>();
            parts
                .into_iter()
                .fold(0.0, |total, value| total * 60.0 + value)
        })
        .sum();
    (total, pids.len())
}

fn counter_delta(
    before: &BTreeMap<String, u64>,
    after: &BTreeMap<String, u64>,
) -> BTreeMap<String, u64> {
    after
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value.checked_sub(*before.get(key).unwrap_or(&0)).unwrap(),
            )
        })
        .collect()
}

pub fn eight_task_wait(root: &Path, counters: fn() -> BTreeMap<String, u64>) -> Value {
    let state = root.join("wait-component");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let mut store = OrchestrationStore::open(&state).unwrap();
    drop(
        dolgorae::engagement::EngagementStore::open(
            &dolgorae::engagement::EngagementStore::workspace_database_path(&state),
        )
        .unwrap(),
    );
    let session = Uuid::now_v7();
    let artifact_root = state
        .join("runs")
        .join(session.to_string())
        .join("artifacts");
    fs::create_dir_all(&artifact_root).unwrap();
    for path in [
        state.clone(),
        state.join("runs"),
        artifact_root.parent().unwrap().to_owned(),
        artifact_root,
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    store
        .prepare_session(
            "component-workspace",
            session,
            "component-bootstrap",
            &"b".repeat(64),
            &policy(),
        )
        .unwrap();
    store.mark_primary_intent(session).unwrap();
    store.finish_primary_publication(session, Ok(())).unwrap();
    let finished = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicU64::new(0));
    let mut adapter = TaskAdapter {
        finished: finished.clone(),
        observations: polls.clone(),
        target: None,
        completion_created_at: Arc::new(OnceLock::new()),
    };
    let mut tasks = Vec::new();
    for index in 0..8 {
        let member = store
            .request_specialist(
                &call_context(session, &format!("member-{index}")),
                &RequestSpecialist {
                    role_ref: "reviewer".to_owned(),
                    objective: "Independent synthetic wait task.".to_owned(),
                    expected_output: vec!["A bounded result.".to_owned()],
                    requested_access: "read_only".to_owned(),
                    deadline_seconds: 600,
                },
                &mut adapter,
            )
            .unwrap()
            .specialist_run_id
            .unwrap();
        let task = store
            .assign_task(
                &call_context(session, &format!("task-{index}")),
                &AssignSpecialistTask {
                    specialist_run_id: member,
                    objective: "Observe completion without replay.".to_owned(),
                    context_refs: Vec::new(),
                    expected_output: vec!["A bounded result.".to_owned()],
                    requested_access: "read_only".to_owned(),
                    deadline_seconds: 600,
                },
                &mut adapter,
            )
            .unwrap();
        tasks.push(task.task_id);
        PrimaryOrchestrationService {
            store: &mut store,
            adapter: &mut adapter,
        }
        .dispatch(
            &call_context(session, &format!("warm-{index}")),
            &await_payload(&[task.task_id], 1),
        )
        .unwrap();
        assert_eq!(
            store.await_tasks(session, &[task.task_id]).unwrap()[0].state,
            "running"
        );
    }
    adapter.target = Some(tasks[0]);
    drop(store);
    let growth_before = growth(&state);
    let baseline_cpu_start = cumulative_cpu_seconds(root);
    let baseline_counters = counters();
    let baseline_polls = polls.load(Ordering::Relaxed);
    let baseline_started = Instant::now();
    std::thread::sleep(Duration::from_secs(60));
    let baseline_elapsed = baseline_started.elapsed();
    let baseline_cpu_end = cumulative_cpu_seconds(root);
    let baseline_delta = counter_delta(&baseline_counters, &counters());
    let baseline_poll_delta = polls.load(Ordering::Relaxed) - baseline_polls;
    let waiting_cpu_start = cumulative_cpu_seconds(root);
    let waiting_counters = counters();
    let waiting_polls = polls.load(Ordering::Relaxed);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    let waiting_started = Instant::now();
    let waiter_state = state.clone();
    let mut waiter_adapter = adapter.clone();
    let waiter_barrier = barrier.clone();
    let waiter_tasks = tasks.clone();
    let waiter = std::thread::spawn(move || {
        let mut store = OrchestrationStore::open(&waiter_state).unwrap();
        waiter_barrier.wait();
        let result = PrimaryOrchestrationService {
            store: &mut store,
            adapter: &mut waiter_adapter,
        }
        .dispatch(
            &call_context(session, "wait-eight"),
            &await_payload(&waiter_tasks, 60),
        )
        .unwrap();
        completed_tx.send((Instant::now(), result)).unwrap();
        // The first completion wakes Any; retain an actual supported wait for
        // the remaining seven tasks through the complete 60-second interval.
        PrimaryOrchestrationService {
            store: &mut store,
            adapter: &mut waiter_adapter,
        }
        .dispatch(
            &call_context(session, "wait-remaining-seven"),
            &await_payload(&waiter_tasks[1..], 30),
        )
        .unwrap();
    });
    barrier.wait();
    std::thread::sleep(Duration::from_secs(30));
    let scripted = Instant::now();
    adapter
        .completion_created_at
        .set(SystemLedgerClock::default().timestamp())
        .unwrap();
    finished.store(true, Ordering::Release);
    let mut publisher = OrchestrationStore::open(&state).unwrap();
    let mut publisher_adapter = adapter.clone();
    PrimaryOrchestrationService {
        store: &mut publisher,
        adapter: &mut publisher_adapter,
    }
    .dispatch(
        &call_context(session, "scripted-completion"),
        &await_payload(&[tasks[0]], 1),
    )
    .unwrap();
    drop(publisher);
    let (noticed, receipt) = completed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("scripted completion was not observed within two seconds");
    let completion_latency = noticed.duration_since(scripted);
    let remaining = Duration::from_secs(60).saturating_sub(waiting_started.elapsed());
    std::thread::sleep(remaining);
    waiter.join().unwrap();
    let waiting_cpu_end = cumulative_cpu_seconds(root);
    let waiting_delta = counter_delta(&waiting_counters, &counters());
    let waiting_poll_delta = polls.load(Ordering::Relaxed) - waiting_polls;
    let store = OrchestrationStore::open_observer(&state).unwrap();
    assert!(matches!(
        store.await_tasks(session, &[tasks[0]]).unwrap()[0]
            .state
            .as_str(),
        "completed_not_delivered" | "delivered"
    ));
    let pending = tasks
        .iter()
        .skip(1)
        .map(|task| {
            store.await_tasks(session, &[*task]).unwrap()[0]
                .state
                .clone()
        })
        .collect::<Vec<_>>();
    assert!(pending.iter().all(|status| status == "running"));
    drop(store);
    let passed = completion_latency <= Duration::from_secs(1)
        && waiting_started.elapsed() >= Duration::from_secs(60);
    json!({"count":8,"existing_core_tasks":true,"wait_mode":"Any over eight IDs, then remaining seven through full 60-second interval","provider":"deterministic OrchestrationAdapter seam; durable production broker acceptance and reconciliation",
        "baseline":{"duration_ms":baseline_elapsed.as_millis().to_string(),"cpu_seconds":baseline_cpu_end.0-baseline_cpu_start.0,
            "processes_start":baseline_cpu_start.1,"processes_end":baseline_cpu_end.1,"counters":baseline_delta,"adapter_observation_calls":baseline_poll_delta},
        "waiting":{"duration_ms":waiting_started.elapsed().as_millis().to_string(),"cpu_seconds":waiting_cpu_end.0-waiting_cpu_start.0,
            "processes_start":waiting_cpu_start.1,"processes_end":waiting_cpu_end.1,"counters":waiting_delta,"adapter_observation_calls":waiting_poll_delta},
        "scripted_completion_latency_ns":completion_latency.as_nanos().to_string(),"completion_state":receipt["tasks"][0]["state"],
        "remaining_task_states":pending,"growth_before":growth_before,"growth_after":growth(&state),"query_plans":query_plans(&state),"passed":passed})
}

pub fn growth(root: &Path) -> BTreeMap<String, u64> {
    fn visit(path: &Path, totals: &mut BTreeMap<String, u64>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                visit(&entry.path(), totals);
            } else if metadata.is_file() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let class = if name.ends_with("-wal") {
                    "wal_bytes"
                } else if name.ends_with("sqlite3") || name.ends_with(".db") {
                    "database_bytes"
                } else {
                    "other_bytes"
                };
                *totals.entry(class.to_owned()).or_default() += metadata.len();
                *totals.entry("files".to_owned()).or_default() += 1;
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, &mut result);
    result
}

fn query_plans(state: &Path) -> Vec<Value> {
    let path = dolgorae::engagement::EngagementStore::workspace_database_path(state);
    dolgorae::storage_observation::query_plans(&path)
        .unwrap()
        .into_iter()
        .map(|(name, plan)| json!({"query":name,"plan":plan}))
        .collect()
}

fn policy() -> InstalledSpecialistPolicy {
    let role_source = RoleSource {
        schema_version: 1,
        name: "reviewer".to_owned(),
        display_name: "Reviewer".to_owned(),
        description: "Reviews the requested change.".to_owned(),
        instructions: "Inspect the evidence and return a bounded result.".to_owned(),
    };
    let role_source_sha256 = dolgorae::jcs::sha256_hex(
        &dolgorae::jcs::canonicalize(
            &dolgorae::jcs::parse(&serde_json::to_string(&role_source).unwrap()).unwrap(),
        )
        .unwrap(),
    );
    InstalledSpecialistPolicy {
        schema_version: 2,
        policy_name: "component-policy".to_owned(),
        revision: 1,
        approval_policy: "fully_delegated".to_owned(),
        max_active_specialists: 8,
        roles: vec![InstalledSpecialistRole {
            role_ref: "reviewer".to_owned(),
            role_source: RoleSourceReference {
                scope: RoleSourceScope::Project,
                name: "reviewer".to_owned(),
            },
            role_source_sha256,
            role: role_source,
            agent_configuration: AgentConfigurationSnapshot {
                schema_version: 2,
                runtime_profile: "test".to_owned(),
                runtime_profile_snapshot_sha256: "a".repeat(64),
                model: "test-model".to_owned(),
                default_effort: "medium".to_owned(),
                purpose: Purpose {
                    kind: PurposeKind::Review,
                    external_label: None,
                },
                required_capabilities: Vec::new(),
                role_reference: Some("reviewer".to_owned()),
                normalized_instructions: "Inspect the evidence and return a bounded result."
                    .to_owned(),
                instructions: InstructionSnapshot {
                    schema: "dolgorae.instructions/v1".to_owned(),
                    common_prefix_version: 1,
                    mode_prefix_version: 1,
                    purpose_prefix_version: 1,
                    normalized_byte_length: 49,
                    normalized_sha256: dolgorae::jcs::sha256_hex(
                        b"Inspect the evidence and return a bounded result.",
                    ),
                }
                .into(),
                execution_lane: ExecutionLane::Dedicated,
                required_assurance: Assurance::BestEffortPersonalAlpha,
                native_subagent_policy: "enabled".to_owned(),
            },
            max_active_instances: 8,
            reuse_policy: "never".to_owned(),
            allowed_access: vec![
                "read_only".to_owned(),
                "canonical_workspace_write".to_owned(),
            ],
            activation_policy: "keep_resident".to_owned(),
            primary_may_request: true,
            collaboration_source: false,
            collaboration_target: false,
            auto_approve_when_fully_delegated: true,
        }],
    }
}
