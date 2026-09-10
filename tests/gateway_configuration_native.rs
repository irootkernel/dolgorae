#![cfg(target_os = "macos")]

#[path = "support/gateway_native.rs"]
mod support;

use dolgorae::protocol::public_v1 as pb;
use prost::Message;
use serde_json::Value;
use std::time::Duration;
use support::{Fixture, context};

#[derive(Clone, PartialEq, Message)]
struct RichStatus {
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}

fn error_detail(status: &tonic::Status) -> pb::DolgoraeErrorDetail {
    let rich = RichStatus::decode(status.details()).expect("typed gRPC error details");
    let detail = rich
        .details
        .iter()
        .find(|detail| {
            detail.type_url == "type.googleapis.com/dolgorae.public.v1.DolgoraeErrorDetail"
        })
        .expect("Dolgorae error detail");
    pb::DolgoraeErrorDetail::decode(detail.value.as_slice()).unwrap()
}

fn start_request(fixture: &Fixture, lane: pb::ExecutionLane) -> pb::StartRunRequest {
    pb::StartRunRequest {
        context: context(),
        workspace: Some(fixture.workspace()),
        controller: fixture.carrier(),
        idempotency_key: uuid::Uuid::now_v7().to_string(),
        profile_name: fixture.profile.clone(),
        control_mode: pb::ControlMode::ManagedAgent as i32,
        execution_lane: lane as i32,
        purpose: pb::PurposeKind::Implementation as i32,
        purpose_label: Some("configuration acceptance".to_owned()),
        model: Some("gpt-5.6".to_owned()),
        effort: Some("low".to_owned()),
        required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
        required_capabilities: vec!["model_list".to_owned(), "account_read".to_owned()],
        instructions: Some(
            "Preserve this accepted configuration across process restarts.".to_owned(),
        ),
        parent: Some(pb::ParentRefProjection {
            namespace: "gateway-tests".to_owned(),
            kind: "acceptance".to_owned(),
            id: "configuration".to_owned(),
        }),
    }
}

fn worker_record(fixture: &Fixture, run_id: &str) -> dolgorae::worker::WorkerRuntimeRecord {
    let path = fixture
        .state_root
        .join("runtime")
        .join("runs")
        .join(format!("{run_id}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

async fn get_run(
    client: &mut pb::run_service_client::RunServiceClient<tonic::transport::Channel>,
    fixture: &Fixture,
    run_id: &str,
) -> pb::RunProjection {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match client
            .get_run(pb::GetRunRequest {
                context: context(),
                run: fixture.run_ref(run_id),
            })
            .await
        {
            Ok(response) => return response.into_inner().run.unwrap(),
            Err(status) => {
                assert_eq!(
                    error_detail(&status).dolgorae_error_code,
                    "RUN_STATE_CONFLICT"
                );
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "snapshot never converged"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

async fn submit_read(
    client: &mut pb::run_service_client::RunServiceClient<tonic::transport::Channel>,
    fixture: &Fixture,
    run_id: &str,
) {
    let key = uuid::Uuid::now_v7().to_string();
    for _ in 0..3 {
        let current = get_run(client, fixture, run_id).await;
        match client
            .submit_turn(pb::SubmitTurnRequest {
                context: context(),
                run: fixture.run_ref(run_id),
                controller: fixture.carrier(),
                idempotency_key: key.clone(),
                write_intent: pb::WriteIntent::Read as i32,
                message: "Read-only configuration restart test.".to_owned(),
                images: vec![],
                effort: None,
                expected_state_revision: current.state_revision,
            })
            .await
        {
            Ok(_) => {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                while get_run(client, fixture, run_id).await.lifecycle
                    != pb::RunLifecycle::Idle as i32
                {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "fake Turn did not complete"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                return;
            }
            Err(status) => assert_eq!(
                error_detail(&status).dolgorae_error_code,
                "RUN_STATE_CONFLICT"
            ),
        }
    }
    panic!("read Turn did not converge after worker startup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_configuration_restart_e2e() {
    let fixture = Fixture::new("run_start_model_list.json");
    let mut gateway = fixture.start_gateway();
    let mut client = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let accepted = client
        .start_run(start_request(&fixture, pb::ExecutionLane::SharedReadonly))
        .await
        .unwrap()
        .into_inner();
    let instance = accepted.context.unwrap().server_instance_id;
    let initial = accepted.run.unwrap();
    let configuration = initial.configuration.clone().unwrap();
    assert_eq!(configuration.profile_name, fixture.profile);
    assert_eq!(configuration.model_id, "gpt-5.6");
    assert_eq!(
        configuration.purpose,
        pb::PurposeKind::Implementation as i32
    );
    assert_eq!(
        configuration.purpose_label.as_deref(),
        Some("configuration acceptance")
    );
    assert_eq!(configuration.default_effort, "low");
    assert_eq!(
        configuration.required_capabilities,
        ["account_read", "model_list"]
    );
    assert_eq!(configuration.parent.as_ref().unwrap().id, "configuration");
    assert!(configuration.controller_instructions_byte_length > 0);
    assert_eq!(configuration.controller_instructions_sha256.len(), 64);

    submit_read(&mut client, &fixture, &initial.run_id).await;
    assert_eq!(
        get_run(&mut client, &fixture, &initial.run_id)
            .await
            .configuration
            .as_ref(),
        Some(&configuration)
    );
    gateway.terminate();
    let mut replacement = fixture.start_gateway();
    client = pb::run_service_client::RunServiceClient::new(replacement.channel().await);
    let restarted = client
        .get_run(pb::GetRunRequest {
            context: context(),
            run: fixture.run_ref(&initial.run_id),
        })
        .await
        .unwrap()
        .into_inner();
    assert_ne!(restarted.context.unwrap().server_instance_id, instance);
    assert_eq!(
        restarted.run.unwrap().configuration.as_ref(),
        Some(&configuration)
    );

    // Kill only the exact worker recorded under this isolated fixture's home.
    let old_worker = worker_record(&fixture, &initial.run_id);
    assert_eq!(
        dolgorae::worker::classify_worker_identity(&old_worker),
        dolgorae::worker::ProcessIdentityVerdict::Match
    );
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-KILL", &old_worker.identity.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while dolgorae::worker::classify_worker_identity(&old_worker)
        != dolgorae::worker::ProcessIdentityVerdict::Absent
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "owned worker did not exit"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        get_run(&mut client, &fixture, &initial.run_id)
            .await
            .configuration
            .as_ref(),
        Some(&configuration)
    );
    submit_read(&mut client, &fixture, &initial.run_id).await;
    let changed = get_run(&mut client, &fixture, &initial.run_id).await;
    let new_worker = worker_record(&fixture, &initial.run_id);
    assert_ne!(new_worker.identity.pid, old_worker.identity.pid);
    let expected = configuration;
    assert_eq!(changed.configuration.as_ref(), Some(&expected));
    replacement.terminate();
    let mut final_gateway = fixture.start_gateway();
    let mut final_client =
        pb::run_service_client::RunServiceClient::new(final_gateway.channel().await);
    assert_eq!(
        get_run(&mut final_client, &fixture, &initial.run_id)
            .await
            .configuration
            .as_ref(),
        Some(&expected)
    );
    final_gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threadless_submit_writer_activation_e2e() {
    let fixture = Fixture::new("run_start_model_list.json");
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut writers = pb::writer_service_client::WriterServiceClient::new(channel);
    let initial = runs
        .start_run(start_request(&fixture, pb::ExecutionLane::Dedicated))
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert!(initial.thread.is_none());
    assert!(initial.active_turn.is_none());
    assert_eq!(
        initial.state_variant,
        pb::RunStateVariant::DedicatedUnstarted as i32
    );
    assert_eq!(
        initial.writer_authority.as_ref().unwrap().state,
        pb::WriterAuthorityState::None as i32
    );
    assert!(
        !fixture
            .state_root
            .join("runtime/runs")
            .join(format!("{}.json", initial.run_id))
            .exists()
    );
    let transcript_before = std::fs::read_to_string(fixture.root.join("transcript.jsonl")).unwrap();
    assert!(
        !transcript_before
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .any(|message| matches!(
                message["method"].as_str(),
                Some("thread/start" | "turn/start")
            ))
    );
    let audit_path = fixture
        .state_root
        .join("runs")
        .join(&initial.run_id)
        .join("audit.jsonl");
    let before: Vec<Value> = std::fs::read_to_string(&audit_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!before.iter().any(|record| matches!(
        record["kind"].as_str(),
        Some("thread_bound" | "turn_started")
    )));

    let refusal = writers
        .acquire_writer(pb::AcquireWriterRequest {
            context: context(),
            run: fixture.run_ref(&initial.run_id),
            controller: fixture.carrier(),
            expected_state_revision: initial.state_revision,
        })
        .await
        .unwrap_err();
    assert_eq!(
        error_detail(&refusal).dolgorae_error_code,
        "THREADLESS_REQUIRES_WRITE_TURN"
    );
    let current = get_run(&mut runs, &fixture, &initial.run_id).await;
    let accepted = runs
        .submit_turn(pb::SubmitTurnRequest {
            context: context(),
            run: fixture.run_ref(&initial.run_id),
            controller: fixture.carrier(),
            idempotency_key: uuid::Uuid::now_v7().to_string(),
            write_intent: pb::WriteIntent::Write as i32,
            message: "Perform the isolated first write turn.".to_owned(),
            images: vec![],
            effort: None,
            expected_state_revision: current.state_revision,
        })
        .await
        .unwrap()
        .into_inner();
    let turn = accepted.accepted_turn.unwrap();
    let run = accepted.run.unwrap();
    let writer = accepted.writer.unwrap();
    assert_eq!(turn.status, pb::TurnStatus::Accepted as i32);
    assert_eq!(run.lifecycle, pb::RunLifecycle::Running as i32);
    assert_eq!(run.thread.as_ref().unwrap().thread_id, turn.thread_id);
    assert_eq!(run.active_turn.as_ref().unwrap().turn_id, turn.turn_id);
    assert_eq!(
        writer.authority_state,
        pb::WriterAuthorityState::Active as i32
    );
    assert_eq!(
        writer.owner_run_id.as_deref(),
        Some(initial.run_id.as_str())
    );
    assert!(writer.writer_generation > 0);
    assert_eq!(run.stamp, writer.stamp);
    assert!(
        worker_record(&fixture, &initial.run_id)
            .dedicated_server_identity
            .is_some()
    );
    let after: Vec<Value> = std::fs::read_to_string(audit_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let acquired = after
        .iter()
        .find(|record| record["kind"] == "writer_acquired")
        .expect("durable writer activation");
    let started = after
        .iter()
        .find(|record| record["kind"] == "turn_started")
        .expect("durable Turn acceptance");
    assert!(acquired["sequence"].as_u64().unwrap() < started["sequence"].as_u64().unwrap());
    let transcript_after = std::fs::read_to_string(fixture.root.join("transcript.jsonl")).unwrap();
    let turn_request = transcript_after
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|message| message["method"] == "turn/start")
        .expect("actual fake runtime received first Turn");
    assert_eq!(
        turn_request["params"]["sandboxPolicy"]["type"],
        "workspaceWrite"
    );
    gateway.terminate();
}
