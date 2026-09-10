//! Required public interaction scenarios against the production local gateway.
#![cfg(target_os = "macos")]
#[path = "support/gateway_native.rs"]
mod support;

use dolgorae::protocol::public_v1 as pb;
use pb::interaction_service_client::InteractionServiceClient;
use pb::run_service_client::RunServiceClient;
use prost::Message;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};
use support::{Fixture, base_scenario, context};

#[derive(Clone, PartialEq, Message)]
struct RichStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}

fn semantic_error(status: &tonic::Status, expected: &str) -> pb::DolgoraeErrorDetail {
    let rich = RichStatus::decode(status.details()).unwrap();
    assert_eq!(rich.code, status.code() as i32);
    assert_eq!(rich.details.len(), 1);
    let detail = pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.dolgorae_error_code, expected);
    detail
}

fn assert_cli_failure(
    fixture: &Fixture,
    args: &[&str],
    expected_status: i32,
    expected_code: &str,
) -> Value {
    let output = fixture.command(args);
    assert_eq!(output.status.code(), Some(expected_status));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], expected_code);
    envelope["error"].clone()
}

fn secret_scenario() -> Value {
    let mut scenario = base_scenario();
    for step in scenario["steps"].as_array_mut().unwrap() {
        if step["method"] == "turn/start" && step["occurrence"] == 1 {
            step["emit"] = json!([
                {"kind":"request","id":7001,"method":"item/tool/requestUserInput","params":{
                    "threadId":"${thread_id}","turnId":"turn-1","questions":[{
                        "id":"credential","header":"Credential","question":"Enter the protected value",
                        "isSecret":true,"isOther":true,"options":[]
                    }]
                }},
                {"kind":"notification","await_reply":true,"method":"turn/completed","params":{
                    "threadId":"${thread_id}","turn":{"id":"turn-1","status":"completed","items":[]}
                }}
            ]);
        }
    }
    scenario
}

async fn pending(
    client: &mut InteractionServiceClient<tonic::transport::Channel>,
    fixture: &Fixture,
    run: &str,
) -> pb::InteractionSummary {
    let deadline = Instant::now() + Duration::from_secs(15);
    let state_path = fixture.state_root.join("runs").join(run).join("state.json");
    loop {
        // The public approval record precedes the durable lifecycle record.
        // Compare rejection revisions only after that opening has completed.
        let state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        if state["lifecycle"] != "waiting_interaction" {
            assert!(
                Instant::now() < deadline,
                "interaction opening did not settle"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
            continue;
        }
        let response = client
            .list_pending_interactions(pb::ListPendingInteractionsRequest {
                context: context(),
                run: fixture.run_ref(run),
            })
            .await
            .unwrap()
            .into_inner();
        if let Some(summary) = response.items.into_iter().next() {
            return summary;
        }
        assert!(
            Instant::now() < deadline,
            "protected interaction did not become pending"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

fn assert_tree_excludes(path: &Path, canary: &[u8]) {
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            assert_tree_excludes(&entry.path(), canary);
        } else if kind.is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(
                !bytes.windows(canary.len()).any(|value| value == canary),
                "secret persisted in {}",
                entry.path().display()
            );
        }
    }
}

async fn submit(
    client: &mut RunServiceClient<tonic::transport::Channel>,
    fixture: &Fixture,
    run: &str,
) {
    let snapshot = client
        .get_run(pb::GetRunRequest {
            context: context(),
            run: fixture.run_ref(run),
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    client
        .submit_turn(pb::SubmitTurnRequest {
            context: context(),
            run: fixture.run_ref(run),
            controller: fixture.carrier(),
            idempotency_key: "secret-turn".into(),
            write_intent: pb::WriteIntent::Read as i32,
            message: "request protected input".into(),
            images: vec![],
            effort: None,
            expected_state_revision: snapshot.state_revision,
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secret_canary_and_fault_barrier() {
    let fixture = Fixture::with_scenario(secret_scenario());
    let run = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = RunServiceClient::new(channel.clone());
    let mut interactions = InteractionServiceClient::new(channel);
    submit(&mut runs, &fixture, &run).await;
    let summary = pending(&mut interactions, &fixture, &run).await;
    assert!(summary.contains_protected_input);
    assert_eq!(summary.kind, pb::InteractionKind::UserInput as i32);
    let observed = fixture.cli(&[
        "run",
        "pending",
        &run,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    let items = observed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["request_id"], summary.interaction_id);
    assert_eq!(items[0]["title"], summary.safe_title);
    assert_eq!(
        items[0]["protected_input"],
        summary.contains_protected_input
    );
    assert_eq!(
        items[0]["user_escalation_required"],
        summary.requires_user_escalation
    );
    let canary = format!("secret-canary-{}", uuid::Uuid::now_v7());
    let id = summary.interaction_id;
    let authorized = interactions
        .get_controller_interaction(pb::GetControllerInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: fixture.carrier(),
            interaction_id: id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(authorized.interaction.unwrap().payload.is_some());
    // The returned receipt is deliberately discarded. The durable resolved
    // summary is the fault barrier; the replacement client never re-enters the secret.
    drop(
        interactions
            .resolve_interaction(pb::ResolveInteractionRequest {
                context: context(),
                run: fixture.run_ref(&run),
                controller: fixture.carrier(),
                interaction_id: id.clone(),
                idempotency_key: "secret-resolution".into(),
                response_json: serde_json::to_vec(
                    &json!({"answers":{"credential":{"answers":[canary]}}}),
                )
                .unwrap(),
            })
            .await
            .unwrap(),
    );
    drop(interactions);
    drop(runs);
    gateway.kill();
    let mut replacement = fixture.start_gateway();
    let channel = replacement.channel().await;
    let mut interactions = InteractionServiceClient::new(channel.clone());
    let resolved = interactions
        .get_controller_interaction(pb::GetControllerInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: fixture.carrier(),
            interaction_id: id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resolved
            .interaction
            .as_ref()
            .unwrap()
            .summary
            .as_ref()
            .unwrap()
            .status,
        pb::InteractionStatus::Resolved as i32
    );
    assert!(
        !resolved
            .encode_to_vec()
            .windows(canary.len())
            .any(|value| value == canary.as_bytes())
    );
    let receipt = interactions
        .resolve_interaction(pb::ResolveInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: fixture.carrier(),
            interaction_id: id.clone(),
            idempotency_key: "secret-resolution".into(),
            response_json: b"{}".to_vec(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!receipt.resolution_receipt.is_empty());
    let again = interactions
        .resolve_interaction(pb::ResolveInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: fixture.carrier(),
            interaction_id: id,
            idempotency_key: "secret-resolution".into(),
            response_json: b"{}".to_vec(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(receipt.resolution_receipt, again.resolution_receipt);
    let transcript = std::fs::read_to_string(fixture.root.join("transcript.jsonl")).unwrap();
    assert_eq!(
        transcript.matches(&canary).count(),
        1,
        "protected value was sent more than once"
    );
    assert_tree_excludes(&fixture.home.join(".dolgorae"), canary.as_bytes());
    let mut observations = pb::observation_service_client::ObservationServiceClient::new(channel);
    let mut stream = observations
        .watch_run_events(pb::WatchRunEventsRequest {
            context: context(),
            run: fixture.run_ref(&run),
            after_cursor: String::new(),
            projection: pb::ProjectionProfile::Operational as i32,
            projection_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    for _ in 0..32 {
        match tokio::time::timeout(Duration::from_millis(300), stream.message()).await {
            Ok(Ok(Some(event))) => assert!(
                !event
                    .encode_to_vec()
                    .windows(canary.len())
                    .any(|value| value == canary.as_bytes())
            ),
            Ok(Err(error)) => panic!("event replay failed: {error}"),
            _ => break,
        }
    }
    replacement.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preparse_bound_and_no_secret_replay_e2e() {
    let fixture = Fixture::with_scenario(secret_scenario());
    let run = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = RunServiceClient::new(channel.clone());
    let mut interactions = InteractionServiceClient::new(channel);
    submit(&mut runs, &fixture, &run).await;
    let summary = pending(&mut interactions, &fixture, &run).await;
    let revision = summary.state_revision;
    let interaction = fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "interaction",
        "get",
        &run,
        &summary.interaction_id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert_eq!(interaction["request_id"], summary.interaction_id);
    assert_eq!(interaction["run_id"], run);
    let rejected = assert_cli_failure(
        &fixture,
        &[
            "run",
            "interaction",
            "get",
            &run,
            &summary.interaction_id,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
        ],
        4,
        "INTERACTION_FULL_PAYLOAD_REQUIRES_CONTROLLER",
    );
    assert_eq!(rejected["details"]["run_id"], run);
    assert_eq!(rejected["details"]["operation"], "run.interaction.get");

    let cli_failure = |controller: &Path, request_id: &str, expected_status| {
        let output = fixture.command(&[
            "run",
            "--controller-file",
            controller.to_str().unwrap(),
            "interaction",
            "get",
            &run,
            request_id,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
        ]);
        assert_eq!(output.status.code(), Some(expected_status));
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["ok"], false);
        envelope["error"].clone()
    };
    let missing_id = uuid::Uuid::now_v7().to_string();
    let missing = cli_failure(&fixture.controller, &missing_id, 3);
    assert_eq!(missing["code"], "INTERACTION_NOT_FOUND");
    assert_eq!(missing["details"]["request_id"], missing_id);

    let wrong_parent = fixture
        .home
        .join(".dolgorae/controller-carriers/gateway-native/wrong-installation");
    std::fs::create_dir(&wrong_parent).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&wrong_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let wrong_controller = wrong_parent.join("controller.json");
    let created = fixture.command(&[
        "controller",
        "credential",
        "create",
        "--kind",
        "automation",
        "--instance-id",
        "gateway-native-wrong-controller",
        "--output",
        wrong_controller.to_str().unwrap(),
    ]);
    assert!(created.status.success());
    let rejected = cli_failure(&wrong_controller, &summary.interaction_id, 4);
    assert_eq!(rejected["code"], "CONTROLLER_MISMATCH");
    assert!(!rejected["message"].as_str().unwrap().contains("generation"));
    let error = interactions
        .resolve_interaction(pb::ResolveInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: fixture.carrier(),
            interaction_id: summary.interaction_id.clone(),
            idempotency_key: "oversized-secret".into(),
            response_json: vec![b'!'; 1_048_577],
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::ResourceExhausted);
    assert!(
        !error.details().is_empty(),
        "bounded parse failure must carry typed details"
    );
    let current = pending(&mut interactions, &fixture, &run).await;
    assert_eq!(current.interaction_id, summary.interaction_id);
    assert_eq!(
        current.state_revision, revision,
        "oversized response changed durable interaction state"
    );
    let error = interactions
        .get_controller_interaction(pb::GetControllerInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: None,
            interaction_id: summary.interaction_id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::PermissionDenied);
    let detail = semantic_error(&error, "INTERACTION_FULL_PAYLOAD_REQUIRES_CONTROLLER");
    assert_eq!(detail.run_id.as_deref(), Some(run.as_str()));
    let mut carrier = fixture.carrier().unwrap();
    carrier.expected_controller_generation += 1;
    let error = interactions
        .get_controller_interaction(pb::GetControllerInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: Some(carrier),
            interaction_id: summary.interaction_id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::PermissionDenied);
    assert!(!error.message().contains("generation"));
    interactions
        .resolve_interaction(pb::ResolveInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: fixture.carrier(),
            interaction_id: summary.interaction_id,
            idempotency_key: "settle-before-ledger-fixture".into(),
            response_json: serde_json::to_vec(
                &json!({"answers":{"credential":{"answers":["bounded"]}}}),
            )
            .unwrap(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state = runs
            .get_run(pb::GetRunRequest {
                context: context(),
                run: fixture.run_ref(&run),
            })
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap();
        if state.lifecycle == pb::RunLifecycle::Idle as i32 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "resolved run did not become idle"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn artifact_download_preserves_bytes_and_reports_range_and_integrity_failures() {
    let text = "🦀".repeat(262145);
    let mut scenario = base_scenario();
    for step in scenario["steps"].as_array_mut().unwrap() {
        if step["method"] == "turn/start" && step["occurrence"] == 1 {
            step["emit"] = json!([{"kind":"notification","method":"turn/completed","params":{"threadId":"${thread_id}","turn":{"id":"turn-1","status":"completed","items":[{"type":"agentMessage","status":"completed","phase":"final_answer","text":text}]}}}]);
        }
    }
    let fixture = Fixture::with_scenario(scenario);
    let run = fixture.start_run(&[]);
    let gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = RunServiceClient::new(channel.clone());
    submit(&mut runs, &fixture, &run).await;
    let deadline = Instant::now() + Duration::from_secs(15);
    let reference = loop {
        let run = runs
            .get_run(pb::GetRunRequest {
                context: context(),
                run: fixture.run_ref(&run),
            })
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap();
        if let Some(pb::FinalResponse {
            value: Some(pb::final_response::Value::Artifact(reference)),
        }) = run.last_final_response
        {
            break reference;
        }
        assert!(Instant::now() < deadline, "final artifact did not appear");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    let mut artifacts = pb::artifact_service_client::ArtifactServiceClient::new(channel);
    let metadata = artifacts
        .get_artifact(pb::GetArtifactRequest {
            context: context(),
            run: fixture.run_ref(&run),
            artifact_id: reference.artifact_id.clone(),
            controller: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(metadata.artifact.as_ref(), Some(&reference));
    let machine_metadata = fixture.cli(&[
        "run",
        "artifact",
        "show",
        &run,
        &reference.artifact_id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    let checked: dolgorae::event::ArtifactMetadata =
        serde_json::from_value(machine_metadata.clone()).unwrap();
    assert_eq!(checked.schema_version, 1);
    assert_eq!(checked.run_id.to_string(), run);
    assert_eq!(checked.artifact_id.to_string(), reference.artifact_id);
    assert_eq!(checked.kind, "final_response");
    assert_eq!(checked.visibility, "observer");
    assert!(checked.interaction_request_id.is_none());
    assert_eq!(checked.media_type, reference.media_type);
    assert_eq!(checked.byte_length, reference.byte_length);
    assert_eq!(checked.sha256, reference.sha256);
    assert!(dolgorae::gateway_event::timestamp(&checked.created_at).is_ok());
    assert_eq!(checked.retention, "run_lifetime");
    assert_eq!(checked.integrity, "verified");
    let machine_chunk = fixture.cli(&[
        "run",
        "artifact",
        "read",
        &run,
        &reference.artifact_id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--offset",
        "1",
        "--length",
        "65537",
    ]);
    assert_eq!(machine_chunk.as_object().unwrap().len(), 5);
    assert_eq!(machine_chunk["metadata"], machine_metadata);
    assert_eq!(machine_chunk["offset"], 1);
    assert_eq!(machine_chunk["byte_length"], 65537);
    assert_eq!(machine_chunk["eof"], false);
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(machine_chunk["content_base64"].as_str().unwrap())
        .unwrap();
    let grpc_chunk = artifacts
        .read_artifact_chunk(pb::ReadArtifactChunkRequest {
            context: context(),
            run: fixture.run_ref(&run),
            artifact_id: reference.artifact_id.clone(),
            offset: 1,
            length: 65537,
            controller: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(decoded, grpc_chunk.data);
    assert_eq!(decoded, &text.as_bytes()[1..65538]);
    let unknown_id = uuid::Uuid::now_v7().to_string();
    let missing = assert_cli_failure(
        &fixture,
        &[
            "run",
            "artifact",
            "show",
            &run,
            &unknown_id,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
        ],
        3,
        "ARTIFACT_NOT_FOUND",
    );
    assert_eq!(missing["details"]["artifact_id"], unknown_id);
    let beyond_end = (reference.byte_length + 1).to_string();
    assert_cli_failure(
        &fixture,
        &[
            "run",
            "artifact",
            "read",
            &run,
            &reference.artifact_id,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--offset",
            &beyond_end,
            "--length",
            "1",
        ],
        2,
        "ARTIFACT_RANGE_INVALID",
    );
    assert_cli_failure(
        &fixture,
        &[
            "run",
            "artifact",
            "read",
            &run,
            &reference.artifact_id,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--offset",
            "0",
            "--length",
            "4294967296",
        ],
        2,
        "INVALID_ARGUMENT",
    );
    let request = |offset, length| pb::ReadArtifactChunkRequest {
        context: context(),
        run: fixture.run_ref(&run),
        artifact_id: reference.artifact_id.clone(),
        offset,
        length,
        controller: None,
    };
    let mut downloaded = Vec::new();
    loop {
        let chunk = artifacts
            .read_artifact_chunk(request(downloaded.len() as u64, 65537))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(chunk.offset, downloaded.len() as u64);
        assert_eq!(chunk.length as usize, chunk.data.len());
        assert_eq!(chunk.sha256, reference.sha256);
        downloaded.extend_from_slice(&chunk.data);
        if chunk.eof {
            break;
        }
    }
    assert_eq!(downloaded, text.as_bytes());
    assert_eq!(dolgorae::jcs::sha256_hex(&downloaded), reference.sha256);
    let eof = artifacts
        .read_artifact_chunk(request(reference.byte_length, 1))
        .await
        .unwrap()
        .into_inner();
    assert!(eof.eof && eof.data.is_empty());
    let error = artifacts
        .read_artifact_chunk(request(reference.byte_length + 1, 1))
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("ARTIFACT_RANGE_INVALID"));
    let error = artifacts
        .get_artifact(pb::GetArtifactRequest {
            context: context(),
            run: fixture.run_ref(&run),
            artifact_id: uuid::Uuid::now_v7().to_string(),
            controller: None,
        })
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("ARTIFACT_NOT_FOUND"));
    let path = fixture
        .state_root
        .join("runs")
        .join(&run)
        .join("artifacts")
        .join(format!("{}.bin", reference.artifact_id));
    *downloaded.last_mut().unwrap() ^= 1;
    std::fs::write(path, downloaded).unwrap();
    let error = artifacts
        .read_artifact_chunk(request(0, 1))
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("ARTIFACT_INTEGRITY_FAILURE"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_change_artifact_requires_current_controller() {
    let mut scenario = base_scenario();
    let diff = format!("@@ -1 +1 @@\n-old\n+{}\n", "x".repeat(32769));
    for step in scenario["steps"].as_array_mut().unwrap() {
        if step["method"] == "turn/start" && step["occurrence"] == 1 {
            step["emit"] = json!([
                {"kind":"notification","method":"item/started","params":{"threadId":"${thread_id}","turnId":"turn-1","startedAtMs":1000,"item":{"type":"fileChange","id":"file-1","status":"inProgress","changes":[{"path":"qa.txt","kind":{"type":"update","move_path":null},"diff":diff},{"path":"qa2.txt","kind":{"type":"update","move_path":null},"diff":diff}]}}},
                {"kind":"request","id":7100,"method":"item/fileChange/requestApproval","params":{"threadId":"${thread_id}","turnId":"turn-1","itemId":"file-1","reason":null,"grantRoot":null}},
                {"kind":"notification","await_reply":true,"method":"turn/completed","params":{"threadId":"${thread_id}","turn":{"id":"turn-1","status":"completed","items":[]}}}
            ]);
        }
    }
    let fixture = Fixture::with_scenario(scenario);
    let run = fixture.start_run(&["--execution-lane", "dedicated"]);
    let gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = RunServiceClient::new(channel.clone());
    let snapshot = runs
        .get_run(pb::GetRunRequest {
            context: context(),
            run: fixture.run_ref(&run),
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    let mut request = pb::SubmitTurnRequest {
        context: context(),
        run: fixture.run_ref(&run),
        controller: fixture.carrier(),
        idempotency_key: "artifact-write".into(),
        write_intent: pb::WriteIntent::Write as i32,
        message: "request file change".into(),
        images: vec![],
        effort: None,
        expected_state_revision: snapshot.state_revision,
    };
    let accepted = runs
        .submit_turn(request.clone())
        .await
        .unwrap()
        .into_inner();
    let mut interactions = InteractionServiceClient::new(channel.clone());
    let summary = pending(&mut interactions, &fixture, &run).await;
    request.context = context();
    request.expected_state_revision = 0;
    let replay = runs.submit_turn(request).await.unwrap().into_inner();
    assert_eq!(replay.accepted_turn, accepted.accepted_turn);
    assert_eq!(replay.run, accepted.run);
    assert_eq!(replay.writer, accepted.writer);
    assert_eq!(replay.correlation_id, accepted.correlation_id);
    let value = interactions
        .get_controller_interaction(pb::GetControllerInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: fixture.carrier(),
            interaction_id: summary.interaction_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    let Some(pb::controller_interaction::Payload::FileChangeApproval(value)) =
        value.interaction.unwrap().payload
    else {
        panic!("expected file change approval");
    };
    let Some(pb::file_change_approval_interaction::Representation::ChangeArtifact(reference)) =
        value.representation
    else {
        panic!("expected large diff artifact");
    };
    let mut artifacts = pb::artifact_service_client::ArtifactServiceClient::new(channel);
    let request = |controller| pb::GetArtifactRequest {
        context: context(),
        run: fixture.run_ref(&run),
        artifact_id: reference.artifact_id.clone(),
        controller,
    };
    let error = artifacts.get_artifact(request(None)).await.unwrap_err();
    assert!(format!("{error:?}").contains("INTERACTION_ARTIFACT_REQUIRES_CONTROLLER"));
    assert_cli_failure(
        &fixture,
        &[
            "run",
            "artifact",
            "show",
            &run,
            &reference.artifact_id,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
        ],
        4,
        "INTERACTION_ARTIFACT_REQUIRES_CONTROLLER",
    );
    let machine = fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "artifact",
        "show",
        &run,
        &reference.artifact_id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert_eq!(machine["artifact_id"], reference.artifact_id);
    assert_eq!(
        artifacts
            .get_artifact(request(fixture.carrier()))
            .await
            .unwrap()
            .into_inner()
            .artifact,
        Some(reference)
    );
    interactions
        .resolve_interaction(pb::ResolveInteractionRequest {
            context: context(),
            run: fixture.run_ref(&run),
            controller: fixture.carrier(),
            interaction_id: summary.interaction_id,
            idempotency_key: "decline-artifact".into(),
            response_json: br#"{"decision":"decline"}"#.to_vec(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = runs
            .get_run(pb::GetRunRequest {
                context: context(),
                run: fixture.run_ref(&run),
            })
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap();
        if snapshot.lifecycle == pb::RunLifecycle::Idle as i32 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "file-change turn did not finish after decline"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
