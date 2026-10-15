//! Explicit, isolated TASK-075 component measurements. This is a fake-runtime
//! campaign, not account, actual Gul, device or paired-host qualification.
#![cfg(target_os = "macos")]

#[path = "observation_campaign.rs"]
mod campaign_support;
use super::support as fixture_support;

use dolgorae::gateway_service::CoreGatewayBackend;
use dolgorae::protocol::public_v1 as pb;
use fixture_support::{Fixture, context};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

const WARM_CALLS: usize = 20;
const MEASURED_CALLS: usize = 200;
const REPETITIONS: usize = 3;
const STREAM_SECONDS: u64 = 600;

fn percentile95(samples: &[u64]) -> u64 {
    assert!(!samples.is_empty());
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    ordered[(95 * ordered.len()).div_ceil(100) - 1]
}

#[test]
fn percentile_uses_nearest_rank_and_keeps_each_repetition() {
    let samples = (1..=200).rev().collect::<Vec<u64>>();
    assert_eq!(percentile95(&samples), 190);
    assert_eq!(percentile95(&[1, 2, 3]), 3);
    assert_eq!(percentile95(&[9]), 9);
}

fn scenario() -> Value {
    let mut document = fixture_support::base_scenario();
    document["concurrent_connections"] = json!(true);
    let steps = document["steps"].as_array_mut().unwrap();
    steps.retain(|step| !matches!(step["method"].as_str(), Some("turn/start" | "model/list")));
    steps.push(json!({"method":"model/list","respond":{"result":{"data":[{
        "model":"gpt-5.6","isDefault":true,"supportedReasoningEfforts":[{"reasoningEffort":"medium"}]
    }],"nextCursor":null}}}));
    for index in 1..=30 {
        steps.push(json!({"method":"turn/start","occurrence":index,
            "respond":{"result":{"turn":{"id":format!("turn-{index}")}}},
            "emit":[{"kind":"notification","method":"turn/completed","params":{
                "threadId":"${thread_id}","turn":{"id":format!("turn-{index}"),"status":"completed",
                "items":[{"type":"agentMessage","phase":"final_answer","status":"completed","text":"Synthetic component response."}]}
            }}]}));
    }
    let turns = (1..=30).map(|index| json!({"id":format!("turn-{index}"),"status":"completed",
        "items":[{"type":"agentMessage","phase":"final_answer","status":"completed","text":"Synthetic component response."}]})).collect::<Vec<_>>();
    steps.push(json!({"method":"thread/read","params_match":{"threadId":"thread-alpha"},"respond":{"result":{"thread":{"id":"${thread_id}","turns":turns}}}}));
    document
}

fn git_fixture(fixture: &Fixture) {
    // The shared fixture starts empty and non-Git. Convert only that exclusive
    // fixture before allocating Runs; no production or caller workspace exists.
    assert_eq!(
        fs::read_dir(fixture.state_root.join("runs"))
            .unwrap()
            .count(),
        0
    );
    fs::remove_dir_all(fixture.workspace.join(".dolgorae")).unwrap();
    fs::remove_dir_all(&fixture.state_root).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Dolgorae Component Fixture"],
        vec!["config", "user.email", "fixture@dolgorae.local"],
        vec!["commit", "--allow-empty", "-qm", "component baseline"],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&fixture.workspace)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    fixture.cli(&["init", fixture.workspace.to_str().unwrap()]);
}

fn add_profiles(fixture: &Fixture) {
    for index in 1..5 {
        fixture.cli(&[
            "profile",
            "add",
            &format!("component-{index}"),
            "--codex-home",
            fixture.root.join("codex-home").to_str().unwrap(),
            "--native-subagents",
            "enabled",
            "--env",
            "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
            "--env",
            "LANG=en_US.UTF-8",
            "--env",
            "LC_ALL=en_US.UTF-8",
            "--",
            fixture.root.join("bin/codex").to_str().unwrap(),
        ]);
    }
}

fn warm_worker(fixture: &Fixture, id: &str, index: usize) {
    // Allocation is intentionally threadless. One completed real fixture Turn
    // establishes a healthy owner before any IDLE observation measurement.
    fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "send",
        id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--message",
        "Establish an idle component fixture worker.",
        "--idempotency-key",
        &format!("component-worker-warm-{index}"),
        "--timeout",
        "30s",
    ]);
    let state = fixture.cli(&[
        "run",
        "status",
        id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert_eq!(state["state"], "idle");
    campaign_support::verify_healthy_worker(&fixture.state_root, Uuid::parse_str(id).unwrap());
}

fn prepare_history(fixture: &Fixture) -> Vec<String> {
    let ids = (0..8).map(|_| fixture.start_run(&[])).collect::<Vec<_>>();
    for index in 0..30 {
        fixture.cli(&[
            "run",
            "--controller-file",
            fixture.controller.to_str().unwrap(),
            "send",
            &ids[0],
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--message",
            &format!("Synthetic component Turn {index}."),
            "--idempotency-key",
            &format!("component-turn-{index}"),
            "--timeout",
            "30s",
        ]);
    }
    campaign_support::augment_timeline_and_restore_worker(fixture, &ids[0], 500);
    for (index, id) in ids.iter().enumerate().skip(1) {
        warm_worker(fixture, id, index);
        campaign_support::assert_turn_history(&fixture.state_root, id, 1);
    }
    assert_eq!(campaign_support::native_turn_dispatches(&fixture.root), 37);
    ids
}

fn absolute_output() -> PathBuf {
    let output = PathBuf::from(
        std::env::var_os("DOLGORAE_CAMPAIGN_OUTPUT").expect("campaign output required"),
    );
    assert!(output.is_absolute());
    assert!(output.parent().unwrap().is_dir());
    output
}

fn isolated_child_case() -> String {
    let thread = std::thread::current();
    match thread.name().and_then(|name| name.rsplit_once("::")) {
        Some((module, _)) => format!("{module}::isolated_campaign_child"),
        None => "isolated_campaign_child".to_owned(),
    }
}

#[test]
fn gateway_observation_reuses_sources_in_an_isolated_process() {
    let fixture = Fixture::with_scenario(scenario());
    git_fixture(&fixture);
    add_profiles(&fixture);
    let ids = (0..8).map(|_| fixture.start_run(&[])).collect::<Vec<_>>();
    for (index, id) in ids.iter().enumerate() {
        warm_worker(&fixture, id, index);
    }
    let config = fixture.root.join("smoke.json");
    let output = fixture.root.join("smoke-result.json");
    fs::write(&config,serde_json::to_vec(&json!({"root":fixture.root,"home":fixture.home,"workspace":fixture.workspace,
        "workspace_id":fixture.workspace_id,"state_root":fixture.state_root,"controller":fixture.controller,
        "controller_id":fixture.controller_id,"run_ids":ids,"output":output})).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &isolated_child_case(), "--nocapture"])
        .env("DOLGORAE_CAMPAIGN_CHILD", &config)
        .env("DOLGORAE_CAMPAIGN_CHILD_MODE", "smoke")
        .env("HOME", &fixture.home)
        .env("TMPDIR", fixture.root.join("tmp"))
        .env("XDG_CONFIG_HOME", fixture.root.join("config"))
        .env("XDG_CACHE_HOME", fixture.root.join("cache"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "isolated source reuse smoke failed: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let evidence: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(evidence["passed"], true);
}

#[test]
#[ignore = "explicit 10-minute streams and 120-second wait campaign; isolated fake runtime"]
fn component_observation_campaign() {
    run_component_campaign("full");
}

#[test]
#[ignore = "explicit isolated warm-read component measurements"]
fn warm_read_component() {
    run_component_campaign("warm");
}

fn run_component_campaign(mode: &str) {
    assert_eq!(
        std::env::var("DOLGORAE_OBSERVATION_CAMPAIGN").as_deref(),
        Ok("1")
    );
    let output = absolute_output();
    assert!(
        !output.exists(),
        "do not replace another campaign's evidence"
    );
    let fixture = Fixture::with_scenario(scenario());
    git_fixture(&fixture);
    add_profiles(&fixture);
    let ids = prepare_history(&fixture);
    let config = fixture.root.join("campaign.json");
    fs::write(&config, serde_json::to_vec(&json!({
        "root":fixture.root,"home":fixture.home,"workspace":fixture.workspace,
        "workspace_id":fixture.workspace_id,"state_root":fixture.state_root,
        "controller":fixture.controller,"controller_id":fixture.controller_id,
        "run_ids":ids,"candidate_binary":fixture.binary,"output":output,
        "source_fingerprint":std::env::var("DOLGORAE_CAMPAIGN_SOURCE_FINGERPRINT").expect("source fingerprint required"),
    })).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &isolated_child_case(), "--nocapture"])
        .env("DOLGORAE_CAMPAIGN_CHILD", &config)
        .env("DOLGORAE_CAMPAIGN_CHILD_MODE", mode)
        .env("HOME", &fixture.home)
        .env("TMPDIR", fixture.root.join("tmp"))
        .env("XDG_CONFIG_HOME", fixture.root.join("config"))
        .env("XDG_CACHE_HOME", fixture.root.join("cache"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "isolated campaign failed: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let evidence: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(
        evidence["accepted"], true,
        "campaign bounds failed; inspect bounded evidence"
    );
}

#[test]
#[ignore = "explicit exact-baseline production gateway latency comparison"]
fn baseline_public_gateway_latency() {
    assert_eq!(
        std::env::var("DOLGORAE_OBSERVATION_CAMPAIGN").as_deref(),
        Ok("1")
    );
    let output = absolute_output();
    assert!(!output.exists());
    let expected = std::env::var("DOLGORAE_CAMPAIGN_EXPECTED_BINARY_SHA256")
        .expect("exact baseline binary digest required");
    let fixture = Fixture::with_scenario(scenario());
    assert_eq!(campaign_support::sha256(&fixture.binary), expected);
    git_fixture(&fixture);
    add_profiles(&fixture);
    let ids = prepare_history(&fixture);
    let selected = Selection {
        workspace: fixture.workspace(),
        controller: fixture.carrier().unwrap(),
        ids,
    };
    let mut gateway = fixture.start_gateway();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (cases, bounds) =
        runtime.block_on(async { warm_reads(gateway.channel().await, &selected, false).await });
    gateway.terminate();
    fs::write(output,serde_json::to_vec_pretty(&json!({"schema":"dolgorae.observation-component-baseline/v1",
        "binary_sha256":expected,"runtime":campaign_support::runtime_configuration(),"warm_reads":cases,"baseline_bounds_passed":bounds,
        "source_counters":"unavailable in uninstrumented historical production server; no zero-work claim",
        "fixture":{"primary_run_turns":30,"other_run_warmup_turns":7,"native_turn_dispatches_before_measurements":37,"timeline_items":500,"profiles":5,"runs":8,"workspace_mode":"git","warm_run_lifecycle":"idle","healthy_worker_verified":true},
        "qualification":"historical component latency comparison only; no current candidate or real-runtime acceptance"})).unwrap()).unwrap();
}

#[test]
#[ignore = "explicit isolated eight-task component wait measurement"]
fn eight_task_wait_component() {
    assert_eq!(
        std::env::var("DOLGORAE_OBSERVATION_CAMPAIGN").as_deref(),
        Ok("1")
    );
    let output = absolute_output();
    assert!(!output.exists());
    let fixture = Fixture::new("run_start_model_list.json");
    let config = fixture.root.join("wait-campaign.json");
    fs::write(&config, serde_json::to_vec(&json!({
        "root":fixture.root,"output":output,
        "source_fingerprint":std::env::var("DOLGORAE_CAMPAIGN_SOURCE_FINGERPRINT").expect("source fingerprint required"),
    })).unwrap()).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &isolated_child_case(), "--nocapture"])
        .env("DOLGORAE_CAMPAIGN_CHILD", &config)
        .env("DOLGORAE_CAMPAIGN_CHILD_MODE", "wait")
        .env("HOME", &fixture.home)
        .env("TMPDIR", fixture.root.join("tmp"))
        .env("XDG_CONFIG_HOME", fixture.root.join("config"))
        .env("XDG_CACHE_HOME", fixture.root.join("cache"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "isolated wait failed: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let evidence: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(evidence["measurements"]["passed"], true);
}

fn counters() -> BTreeMap<String, u64> {
    let mut result = dolgorae::storage_observation::counters()
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect::<BTreeMap<_, _>>();
    let ledger = dolgorae::ledger::observation_counters();
    let profile = dolgorae::profile::profile_observation_counters();
    let workspace = dolgorae::workspace::workspace_observation_counters();
    let process = dolgorae::worker::display_identity_counters();
    for (key, value) in [
        (
            "display_process_executable_hashes",
            process.executable_hashes,
        ),
        (
            "display_process_executable_reuses",
            process.executable_reuses,
        ),
        (
            "authoritative_process_executable_hashes",
            process.authoritative_hashes,
        ),
        ("ledger_full_replays", ledger.full_replays),
        ("ledger_tail_records", ledger.tail_records),
        ("ledger_cache_hits", ledger.cache_hits),
        ("ledger_invalidations", ledger.invalidations),
        ("profile_executable_hashes", profile.executable_hashes),
        ("profile_version_probes", profile.version_probes),
        ("profile_schema_probes", profile.schema_probes),
        ("profile_cache_hits", profile.snapshot_cache_hits),
        ("profile_registry_loads", profile.registry_loads),
        ("git_spawns", workspace.git_spawns),
        ("workspace_cache_hits", workspace.discovery_cache_hits),
        (
            "executing_build_hashes",
            dolgorae::worker::executing_build_hash_count(),
        ),
    ] {
        result.insert(key.to_owned(), value);
    }
    result
}

fn delta(before: &BTreeMap<String, u64>, after: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    after
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .checked_sub(*before.get(key).unwrap_or(&0))
                    .expect("cumulative counter regressed"),
            )
        })
        .collect()
}

struct Selection {
    workspace: pb::WorkspaceRef,
    controller: pb::ControllerCarrierRef,
    ids: Vec<String>,
}
impl Selection {
    fn run_ref(&self, index: usize) -> Option<pb::RunRef> {
        Some(pb::RunRef {
            workspace: Some(self.workspace.clone()),
            run_id: self.ids[index].clone(),
        })
    }
}

async fn channel(socket: &Path) -> tonic::transport::Channel {
    let path = socket.to_owned();
    tonic::transport::Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .initial_stream_window_size(1024)
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                tokio::net::UnixStream::connect(path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .unwrap()
}

async fn warm_reads(
    channel: tonic::transport::Channel,
    selected: &Selection,
    instrumented: bool,
) -> (Vec<Value>, bool) {
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut profiles = pb::runtime_service_client::RuntimeServiceClient::new(channel);
    let mut cases = Vec::new();
    let mut accepted = true;
    for case in [
        "get_run",
        "profile/default",
        "profile/component-1",
        "profile/component-2",
        "profile/component-3",
        "profile/component-4",
        "list_profiles",
        "aggregate",
    ] {
        for _ in 0..WARM_CALLS {
            measured_call(case, &mut runs, &mut profiles, selected).await;
        }
        for repetition in 1..=REPETITIONS {
            let before = counters();
            let started = Instant::now();
            let mut samples = Vec::new();
            for _ in 0..MEASURED_CALLS {
                let call = Instant::now();
                measured_call(case, &mut runs, &mut profiles, selected).await;
                samples.push(u64::try_from(call.elapsed().as_nanos()).unwrap());
            }
            let p95 = percentile95(&samples);
            let bound = if case == "aggregate" {
                100_000_000
            } else {
                50_000_000
            };
            accepted &= p95 <= bound;
            cases.push(json!({"case":case,"repetition":repetition,"warm_calls":WARM_CALLS,
                "count":samples.len(),"samples_ns":samples,"nearest_rank_p95_ns":p95,"bound_ns":bound,
                "elapsed_ns":started.elapsed().as_nanos().to_string(),"counters":if instrumented {Some(delta(&before,&counters()))} else {None},"passed":p95<=bound}));
        }
    }
    (cases, accepted)
}

async fn measured_call(
    case: &str,
    runs: &mut pb::run_service_client::RunServiceClient<tonic::transport::Channel>,
    profiles: &mut pb::runtime_service_client::RuntimeServiceClient<tonic::transport::Channel>,
    selected: &Selection,
) {
    match case {
        "get_run" => {
            let run = runs
                .get_run(pb::GetRunRequest {
                    context: context(),
                    run: selected.run_ref(0),
                })
                .await
                .unwrap()
                .into_inner()
                .run
                .unwrap();
            assert_eq!(run.lifecycle, pb::RunLifecycle::Idle as i32);
        }
        profile if profile.starts_with("profile/") => {
            let name = profile.strip_prefix("profile/").unwrap();
            let observed = profiles
                .get_profile(pb::GetProfileRequest {
                    context: context(),
                    profile_name: name.to_owned(),
                })
                .await
                .unwrap()
                .into_inner()
                .profile
                .unwrap();
            assert_eq!(observed.name, name);
        }
        "list_profiles" => {
            let listed = profiles
                .list_profiles(pb::ListProfilesRequest { context: context() })
                .await
                .unwrap()
                .into_inner();
            let mut names = listed
                .items
                .into_iter()
                .map(|profile| profile.name)
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(
                names,
                [
                    "component-1",
                    "component-2",
                    "component-3",
                    "component-4",
                    "default"
                ]
            );
        }
        "aggregate" => {
            assert_eq!(
                runs.list_runs(pb::ListRunsRequest {
                    context: context(),
                    workspace: Some(selected.workspace.clone()),
                    controller_id: None
                })
                .await
                .unwrap()
                .into_inner()
                .items
                .len(),
                8
            );
        }
        _ => unreachable!(),
    }
}

async fn six_streams(channel: tonic::transport::Channel, selected: &Selection) -> (Value, bool) {
    let mut streams = Vec::new();
    let mut cursors = Vec::new();
    for index in 1..=6 {
        let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
        let run = runs
            .get_run(pb::GetRunRequest {
                context: context(),
                run: selected.run_ref(index),
            })
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap();
        assert_eq!(run.lifecycle, pb::RunLifecycle::Idle as i32);
        let cursor = run.state_revision.to_string();
        let mut observations =
            pb::observation_service_client::ObservationServiceClient::new(channel.clone());
        let stream = observations
            .watch_run_events(pb::WatchRunEventsRequest {
                context: context(),
                run: selected.run_ref(index),
                after_cursor: cursor.clone(),
                projection: pb::ProjectionProfile::Minimal as i32,
                projection_version: 1,
            })
            .await
            .unwrap()
            .into_inner();
        streams.push(stream);
        cursors.push(cursor);
    }
    // Warm all six source observations before taking the steady-idle baseline.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = counters();
    let started = Instant::now();
    let mut tasks = Vec::new();
    for (offset, mut stream) in streams.into_iter().enumerate() {
        let channel = channel.clone();
        let run = selected.run_ref(offset + 1);
        let initial_cursor = cursors[offset].clone();
        tasks.push(tokio::spawn(async move {
            let mut consumed = 0_u64; let mut last = initial_cursor; let mut reconnected = false;
            let mut stall_elapsed = 0_u128;
            while started.elapsed() < Duration::from_secs(STREAM_SECONDS) {
                if offset == 0 && !reconnected && started.elapsed() >= Duration::from_secs(30) {
                    let stall = Instant::now(); tokio::time::sleep(Duration::from_secs(5)).await;
                    stall_elapsed = stall.elapsed().as_millis();
                    drop(stream);
                    let mut client = pb::observation_service_client::ObservationServiceClient::new(channel.clone());
                    stream = client.watch_run_events(pb::WatchRunEventsRequest {context:context(),run:run.clone(),after_cursor:last.clone(),
                        projection:pb::ProjectionProfile::Minimal as i32,projection_version:1}).await.unwrap().into_inner();
                    reconnected = true;
                }
                match tokio::time::timeout(Duration::from_millis(250),stream.message()).await {
                    Ok(Ok(Some(event))) => { consumed+=1; match event.item.unwrap() {
                        pb::run_event_envelope::Item::DurableEvent(event) => last=event.cursor,
                        pb::run_event_envelope::Item::Heartbeat(heartbeat) => last=heartbeat.durable_head_cursor,
                        pb::run_event_envelope::Item::StreamEnd(end) => panic!("idle stream ended: {}",end.reason),
                    } }
                    Ok(Ok(None)) => panic!("idle stream ended before ten minutes"),
                    Ok(Err(error)) => panic!("idle stream failed: {error}"),
                    Err(_) => {}
                }
            }
            json!({"stream":offset+1,"messages":consumed,"reconnected":reconnected,"stall_ms":stall_elapsed,
                "resumed_after_cursor":last,"duration_ms":started.elapsed().as_millis().to_string()})
        }));
    }
    let mut observations = Vec::new();
    for task in tasks {
        observations.push(task.await.unwrap());
    }
    let observed = delta(&before, &counters());
    let passed = observed["git_spawns"] == 0
        && observed["ledger_full_replays"] == 0
        && observations[0]["reconnected"] == true
        && observations[0]["stall_ms"].as_u64().unwrap() >= 5000
        && started.elapsed() >= Duration::from_secs(STREAM_SECONDS);
    (
        json!({"count":6,"duration_ms":started.elapsed().as_millis().to_string(),"streams":observations,"counters":observed,"passed":passed}),
        passed,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn isolated_campaign_child() {
    let Some(config) = std::env::var_os("DOLGORAE_CAMPAIGN_CHILD") else {
        return;
    };
    let document: Value = serde_json::from_slice(&fs::read(config).unwrap()).unwrap();
    let root = PathBuf::from(document["root"].as_str().unwrap());
    assert!(
        root.is_absolute()
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dgg-")
    );
    assert_eq!(
        fs::canonicalize(std::env::var_os("HOME").unwrap()).unwrap(),
        fs::canonicalize(root.join("home")).unwrap()
    );
    if std::env::var("DOLGORAE_CAMPAIGN_CHILD_MODE").as_deref() == Ok("wait") {
        let measurements =
            tokio::task::spawn_blocking(move || campaign_support::eight_task_wait(&root, counters))
                .await
                .unwrap();
        let evidence = json!({"schema":"dolgorae.observation-eight-task-component/v1",
            "source_fingerprint":document["source_fingerprint"],"measurements":measurements,
            "instrumented_test_binary_sha256":campaign_support::sha256(&std::env::current_exe().unwrap()),
            "qualification":"isolated existing core tasks; no account, Gul or full campaign acceptance"});
        fs::write(
            document["output"].as_str().unwrap(),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
        return;
    }
    dolgorae::worker::current_dolgorae_build().unwrap();
    let selected = Selection {
        workspace: pb::WorkspaceRef {
            absolute_path: document["workspace"].as_str().unwrap().to_owned(),
            expected_workspace_id: document["workspace_id"].as_str().unwrap().to_owned(),
        },
        controller: pb::ControllerCarrierRef {
            absolute_file_path: document["controller"].as_str().unwrap().to_owned(),
            expected_controller_id: document["controller_id"].as_str().unwrap().to_owned(),
            expected_controller_generation: 1,
        },
        ids: document["run_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect(),
    };
    for id in &selected.ids {
        campaign_support::verify_healthy_worker(
            Path::new(document["state_root"].as_str().unwrap()),
            Uuid::parse_str(id).unwrap(),
        );
    }
    let home = dolgorae::paths::DolgoraeHome::system().unwrap();
    let socket_path = root.join("c.sock");
    let mut socket = dolgorae::gateway_socket::GatewaySocket::bind(&home, &socket_path).unwrap();
    let backend = Arc::new(CoreGatewayBackend::new(
        home,
        socket.record().server_instance_id,
    ));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        dolgorae::gateway::run_server(&mut socket, backend, async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    let connected = channel(&socket_path).await;
    if std::env::var("DOLGORAE_CAMPAIGN_CHILD_MODE").as_deref() == Ok("smoke") {
        let mut runs = pb::run_service_client::RunServiceClient::new(connected.clone());
        let mut profiles = pb::runtime_service_client::RuntimeServiceClient::new(connected.clone());
        for case in [
            "get_run",
            "profile/default",
            "profile/component-1",
            "profile/component-2",
            "profile/component-3",
            "profile/component-4",
            "list_profiles",
            "aggregate",
        ] {
            measured_call(case, &mut runs, &mut profiles, &selected).await;
        }
        let before = counters();
        for _ in 0..10 {
            for case in [
                "get_run",
                "profile/default",
                "profile/component-1",
                "profile/component-2",
                "profile/component-3",
                "profile/component-4",
                "list_profiles",
                "aggregate",
            ] {
                measured_call(case, &mut runs, &mut profiles, &selected).await;
            }
        }
        let difference = delta(&before, &counters());
        let idle_streams = short_idle_stream_reuse(connected.clone(), &selected).await;
        let passed = difference["git_spawns"] == 0
            && difference["ledger_full_replays"] == 0
            && difference["profile_schema_probes"] == 0
            && difference["profile_version_probes"] == 0
            && difference["profile_executable_hashes"] == 0
            && idle_streams["git_spawns"] == 0
            && idle_streams["ledger_full_replays"] == 0;
        fs::write(document["output"].as_str().unwrap(),serde_json::to_vec(&json!({"passed":passed,"counters":difference,"idle_stream_counters":idle_streams,"mode":"short default source reuse regression; not component campaign acceptance"})).unwrap()).unwrap();
        stop.send(()).unwrap();
        server.await.unwrap();
        display_discovery_reuses_git_and_keeps_integrity_and_mutation_discovery_fresh(&root);
        assert!(
            passed,
            "steady public observations repeated expensive source work"
        );
        return;
    }
    let mut observations =
        pb::observation_service_client::ObservationServiceClient::new(connected.clone());
    let page = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: selected.run_ref(0),
            controller: Some(selected.controller.clone()),
            after_cursor: "0".to_owned(),
            limit: 500,
            timeline_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page.items.len(), 500);
    assert!(page.next_after_cursor.is_none());
    campaign_support::assert_turn_history(
        Path::new(document["state_root"].as_str().unwrap()),
        &selected.ids[0],
        30,
    );
    let (warm, warm_passed) = warm_reads(connected.clone(), &selected, true).await;
    write_campaign_phase(
        &document,
        "warm_reads",
        &json!({"rows":warm,"passed":warm_passed}),
    );
    if std::env::var("DOLGORAE_CAMPAIGN_CHILD_MODE").as_deref() == Ok("warm") {
        fs::write(document["output"].as_str().unwrap(), serde_json::to_vec_pretty(&json!({
            "schema":"dolgorae.observation-warm-read-component/v1",
            "accepted":warm_passed,"campaign_complete":false,
            "source_fingerprint":document["source_fingerprint"],"warm_reads":warm,
            "qualification":"warm-read component only; no full campaign or actual runtime acceptance",
        })).unwrap()).unwrap();
        stop.send(()).unwrap();
        server.await.unwrap();
        return;
    }
    let (streams, streams_passed) = six_streams(connected, &selected).await;
    write_campaign_phase(
        &document,
        "six_streams",
        &json!({"measurements":streams,"passed":streams_passed}),
    );
    let root_for_wait = root.clone();
    let waits = tokio::task::spawn_blocking(move || {
        campaign_support::eight_task_wait(&root_for_wait, counters)
    })
    .await
    .unwrap();
    let accepted = warm_passed && streams_passed && waits["passed"] == true;
    let evidence = json!({"schema":"dolgorae.observation-component-campaign/v1","accepted":accepted,
        "qualification":"isolated native components with fake app-server; actual Gul/account/device/paired-host qualification remains required",
        "candidate_binary_sha256":campaign_support::sha256(Path::new(document["candidate_binary"].as_str().unwrap())),
        "instrumented_test_binary_sha256":campaign_support::sha256(&std::env::current_exe().unwrap()),
        "source_fingerprint":document["source_fingerprint"],"runtime":campaign_support::runtime_configuration(),
        "fixture":{"primary_run_turns":30,"other_run_warmup_turns":7,"native_turn_dispatches_before_measurements":37,"timeline_items":500,"synthetic_extra_timeline_events":410,"profiles":5,"runs":8,"workspace_mode":"git","warm_run_lifecycle":"idle","healthy_worker_verified":true},
        "warm_reads":warm,"six_streams":streams,"eight_task_wait":waits,"fixture_growth_after":campaign_support::growth(&root),"final_counters":counters()});
    fs::write(
        document["output"].as_str().unwrap(),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    stop.send(()).unwrap();
    server.await.unwrap();
}

fn write_campaign_phase(document: &Value, phase: &str, measurements: &Value) {
    let path = PathBuf::from(format!(
        "{}.{}.json",
        document["output"].as_str().unwrap(),
        phase
    ));
    assert!(!path.exists(), "do not replace another phase's evidence");
    let evidence = json!({
        "schema":"dolgorae.observation-component-phase/v1",
        "phase":phase,"campaign_complete":false,
        "source_fingerprint":document["source_fingerprint"],
        "candidate_binary_sha256":campaign_support::sha256(Path::new(document["candidate_binary"].as_str().unwrap())),
        "instrumented_test_binary_sha256":campaign_support::sha256(&std::env::current_exe().unwrap()),
        "measurements":measurements,
    });
    fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
}

async fn short_idle_stream_reuse(
    channel: tonic::transport::Channel,
    selected: &Selection,
) -> BTreeMap<String, u64> {
    let mut streams = Vec::new();
    for index in 1..=6 {
        let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
        let run = runs
            .get_run(pb::GetRunRequest {
                context: context(),
                run: selected.run_ref(index),
            })
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap();
        let mut observations =
            pb::observation_service_client::ObservationServiceClient::new(channel.clone());
        let stream = observations
            .watch_run_events(pb::WatchRunEventsRequest {
                context: context(),
                run: selected.run_ref(index),
                after_cursor: run.state_revision.to_string(),
                projection: pb::ProjectionProfile::Minimal as i32,
                projection_version: 1,
            })
            .await
            .unwrap()
            .into_inner();
        streams.push(stream);
    }
    let before = counters();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let observed = delta(&before, &counters());
    drop(streams);
    observed
}

#[test]
fn snapshot_projects_its_exact_captured_history_and_rejects_display_mutation_admission() {
    use dolgorae::protocol::public_v1 as pb;
    use dolgorae::snapshot::RunSnapshot;
    let fixture = fixture_support::Fixture::new("run_start_model_list.json");
    let run_id_text = fixture.start_run(&[]);
    let run_id = Uuid::parse_str(&run_id_text).unwrap();
    let snapshot = RunSnapshot::load_display(&fixture.state_root, run_id, 0).unwrap();
    let old_head = snapshot.stamp.run_state_revision;
    assert_eq!(
        snapshot
            .require_revision(old_head, "run.pause")
            .unwrap_err()
            .code,
        "RUN_STATE_CONFLICT"
    );
    let fresh = RunSnapshot::load(&fixture.state_root, run_id, 0).unwrap();
    fresh.require_revision(old_head, "run.pause").unwrap();
    let baseline = dolgorae::ledger::observation_counters();
    let old_page = dolgorae::gateway_observation::watch_page(
        &fixture.state_root,
        &snapshot,
        "0",
        pb::ProjectionProfile::Operational as i32,
        1,
    )
    .unwrap();
    assert_eq!(
        dolgorae::ledger::observation_counters().full_replays,
        baseline.full_replays
    );
    fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "send",
        &run_id_text,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--message",
        "snapshot boundary",
        "--idempotency-key",
        "display-boundary-turn",
    ]);
    let newer = RunSnapshot::load_display(&fixture.state_root, run_id, 0).unwrap();
    assert!(newer.stamp.run_state_revision > old_head);
    let baseline = dolgorae::ledger::observation_counters();
    let retained_page = dolgorae::gateway_observation::watch_page(
        &fixture.state_root,
        &snapshot,
        "0",
        pb::ProjectionProfile::Operational as i32,
        1,
    )
    .unwrap();
    assert_eq!(
        retained_page.durable_head_cursor,
        old_page.durable_head_cursor
    );
    assert_eq!(retained_page.events, old_page.events);
    assert_eq!(
        dolgorae::ledger::observation_counters().full_replays,
        baseline.full_replays
    );
    let state_path = fixture
        .state_root
        .join("runs")
        .join(&run_id_text)
        .join("state.json");
    let original = fs::read(&state_path).unwrap();
    let mut projection: serde_json::Value = serde_json::from_slice(&original).unwrap();
    projection["ledger_head"]["hash"] = serde_json::Value::String("0".repeat(64));
    fs::write(&state_path, serde_json::to_vec(&projection).unwrap()).unwrap();
    assert_eq!(
        RunSnapshot::load_display(&fixture.state_root, run_id, 0)
            .unwrap_err()
            .code,
        "RUN_STATE_CONFLICT"
    );
    fs::write(&state_path, original).unwrap();
    assert_eq!(
        RunSnapshot::load_display(&fixture.state_root, run_id, 0)
            .unwrap()
            .stamp,
        newer.stamp
    );
}

#[derive(Clone)]
struct CountedGit(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl dolgorae::workspace::GitRunner for CountedGit {
    fn output(
        &self,
        arguments: &[std::ffi::OsString],
    ) -> Result<std::process::Output, std::io::Error> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        dolgorae::workspace::GitRunner::output(&dolgorae::workspace::SystemGitRunner, arguments)
    }
}

fn display_discovery_reuses_git_and_keeps_integrity_and_mutation_discovery_fresh(root: &Path) {
    use dolgorae::workspace::{SystemWorkspacePlatform, WorkspaceMode, WorkspaceService};
    use std::os::unix::fs::DirBuilderExt as _;
    let case = root.join("discovery");
    let workspace = case.join("workspace");
    let support = case.join("support");
    for path in [&case, &workspace, &support] {
        fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Dolgorae Test"],
        vec!["config", "user.email", "test@dolgorae.local"],
        vec!["commit", "--allow-empty", "-qm", "baseline"],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&workspace)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let support = support.join("Dolgorae");
    let service = WorkspaceService::new(
        SystemWorkspacePlatform,
        CountedGit(calls.clone()),
        support.clone(),
    );
    let view = service
        .initialize(Some(&workspace), WorkspaceMode::Git)
        .unwrap();
    assert_eq!(
        service
            .discover_observed(Some(&workspace))
            .unwrap()
            .workspace_id,
        view.workspace_id
    );
    let warm_calls = calls.load(std::sync::atomic::Ordering::Relaxed);
    for _ in 0..20 {
        service.discover_observed(Some(&workspace)).unwrap();
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), warm_calls);
    service.discover_for_run_start(Some(&workspace)).unwrap();
    assert!(calls.load(std::sync::atomic::Ordering::Relaxed) > warm_calls);
    let before_replace = calls.load(std::sync::atomic::Ordering::Relaxed);
    let git = workspace.join(".git");
    let config = git.join("config");
    let replacement = git.join("config.replacement");
    fs::copy(&config, &replacement).unwrap();
    fs::rename(replacement, &config).unwrap();
    service.discover_observed(Some(&workspace)).unwrap();
    assert!(calls.load(std::sync::atomic::Ordering::Relaxed) > before_replace);
    let lock = support
        .join("workspaces")
        .join(&view.workspace_id)
        .join("runtime/locks");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        service
            .discover_observed(Some(&workspace))
            .unwrap_err()
            .code,
        "RUNTIME_PATH_INVALID"
    );
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(&git, workspace.join("git-moved")).unwrap();
    assert_eq!(
        service
            .discover_observed(Some(&workspace))
            .unwrap_err()
            .code,
        "WORKSPACE_NOT_INITIALIZED"
    );
}
