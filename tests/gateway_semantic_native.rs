//! Public native gateway semantics against isolated durable state and the fake runtime.

#![cfg(target_os = "macos")]

#[path = "support/gateway_native.rs"]
mod support;

use dolgorae::machine::MachineError;
use dolgorae::orchestration::{
    AcceptedSpecialistTask, AcceptedTaskContext, AdapterFailure, AssignSpecialistTask,
    BrokerCredential, BrokeredMemberSnapshot, BrokeredRunPlan, CompletedTask, OrchestrationAdapter,
    OrchestrationStore, PrimaryCallContext, RequestSpecialist, SpecialistPublicationObservation,
    SpecialistTaskObservation, SpecialistTaskSnapshot, TaskCancellation, TaskDispatch,
};
use dolgorae::protocol::public_v1 as pb;
use dolgorae::run::{StartReservation, StartReservationStore};
use dolgorae::workspace::SystemWorkspacePlatform;
use prost::Message;
use serde_json::Value;
use std::fs;
use std::process::Command;
use std::time::Duration;
use support::{Fixture, context};
use tonic::Code;
use uuid::Uuid;

struct PublicResultAdapter;

impl OrchestrationAdapter for PublicResultAdapter {
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
        _task: &AcceptedSpecialistTask,
        _credential: &BrokerCredential,
    ) -> Result<TaskDispatch, AdapterFailure> {
        Ok(TaskDispatch::Completed(CompletedTask {
            turn_id: "public-result-turn".to_owned(),
            result: serde_json::json!({"answer":"공개 결과"}),
        }))
    }

    fn observe_task(
        &mut self,
        _task: &SpecialistTaskSnapshot,
        _credential: &BrokerCredential,
    ) -> Result<SpecialistTaskObservation, AdapterFailure> {
        Ok(SpecialistTaskObservation::Running)
    }

    fn cancel_task(
        &mut self,
        _task: &SpecialistTaskSnapshot,
        _credential: &BrokerCredential,
    ) -> Result<TaskCancellation, AdapterFailure> {
        Ok(TaskCancellation::TerminalOther)
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
    assert_eq!(
        rich.details[0].type_url,
        "type.googleapis.com/dolgorae.public.v1.DolgoraeErrorDetail"
    );
    let detail = pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.detail_version, 1);
    assert_eq!(detail.dolgorae_error_code, expected);
    detail
}

fn start_request(fixture: &Fixture, key: &str, lane: pb::ExecutionLane) -> pb::StartRunRequest {
    pb::StartRunRequest {
        context: context(),
        workspace: Some(fixture.workspace()),
        controller: fixture.carrier(),
        idempotency_key: key.to_owned(),
        profile_name: fixture.profile.clone(),
        control_mode: pb::ControlMode::ManagedAgent as i32,
        execution_lane: lane as i32,
        purpose: pb::PurposeKind::Interactive as i32,
        purpose_label: None,
        model: Some("gpt-5.6".to_owned()),
        effort: Some("medium".to_owned()),
        required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
        required_capabilities: Vec::new(),
        instructions: Some("Exercise the isolated native gateway contract.".to_owned()),
        parent: None,
    }
}

fn run_count(fixture: &Fixture) -> usize {
    fs::read_dir(fixture.state_root.join("runs"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| Uuid::parse_str(name).is_ok())
        })
        .count()
}

fn install_orchestration_controller(
    fixture: &Fixture,
    policy_name: &str,
) -> pb::ControllerCarrierRef {
    install_orchestration_controller_with_approval(
        fixture,
        policy_name,
        "fully_delegated",
        &fixture.profile,
    )
}

fn install_orchestration_controller_with_approval(
    fixture: &Fixture,
    policy_name: &str,
    approval_policy: &str,
    specialist_profile: &str,
) -> pb::ControllerCarrierRef {
    let roles = fixture.workspace.join(".dolgorae/roles");
    fs::create_dir_all(&roles).unwrap();
    fs::write(
        roles.join(format!("{policy_name}-role.json")),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "name": format!("{policy_name}-role"),
            "display_name": "Retirement verifier",
            "description": "Supports deterministic hierarchy retirement tests.",
            "instructions": "Return bounded deterministic results."
        }))
        .unwrap(),
    )
    .unwrap();
    let policy = fixture.root.join(format!("{policy_name}.json"));
    fs::write(
        &policy,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "policy_name": policy_name,
            "revision": 1,
            "approval_policy": approval_policy,
            "max_active_specialists": 1,
            "roles": [{
                "role_ref": format!("{policy_name}-role"),
                "role_source": {"scope":"project","name":format!("{policy_name}-role")},
                "agent_configuration": {
                    "schema_version": 2,
                    "selected_profile": specialist_profile,
                    "global_profile_binding_sha256": null,
                    "model": "gpt-5.6",
                    "default_effort": "medium",
                    "purpose": "review",
                    "purpose_label": null,
                    "required_capabilities": [],
                    "execution_lane": "dedicated",
                    "required_assurance": "best_effort_personal_alpha",
                    "native_subagent_policy": "enabled"
                },
                "max_active_instances": 1,
                "reuse_policy": "never",
                "allowed_access": ["read_only"],
                "activation_policy": "keep_resident",
                "primary_may_request": true,
                "collaboration_source": false,
                "collaboration_target": false,
                "auto_approve_when_fully_delegated": true
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    fixture.cli(&[
        "specialist",
        "policy",
        "add",
        policy_name,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--file",
        policy.to_str().unwrap(),
    ]);
    let controller_path = fixture.home.join(format!(
        ".dolgorae/controller-carriers/gateway-native/test-installation/{policy_name}.json"
    ));
    let created = fixture.cli(&[
        "controller",
        "credential",
        "create",
        "--kind",
        "interactive-client",
        "--instance-id",
        policy_name,
        "--orchestration-policy",
        policy_name,
        "--output",
        controller_path.to_str().unwrap(),
    ]);
    pb::ControllerCarrierRef {
        absolute_file_path: controller_path.to_string_lossy().into_owned(),
        expected_controller_id: created["controller"]["controller_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_controller_generation: 1,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_approval_provisions_and_dispatches_through_production_effects() {
    let mut scenario = support::base_scenario();
    scenario["concurrent_connections"] = true.into();
    for step in scenario["steps"].as_array_mut().unwrap() {
        if step["method"] == "turn/start" {
            if step["occurrence"] == 1 {
                step["emit"] = serde_json::json!([
                    {"kind":"request","id":50,"method":"item/tool/call","params":{
                        "threadId":"${thread_id}","turnId":"${turn_id}","callId":"request-member",
                        "tool":"dolgorae_orchestration","arguments":{
                            "operation":"request_specialist","role_ref":"production-broker-role",
                            "objective":"Return a bounded result.","expected_output":["One result"],
                            "requested_access":"read_only","deadline_seconds":60
                        }
                    }},
                    {"kind":"notification","method":"turn/completed","await_reply":true,"params":{
                        "threadId":"${thread_id}","turn":{
                            "id":"${turn_id}","status":"completed",
                            "items":[{"type":"agentMessage","phase":"final_answer","status":"completed","text":"Production Specialist result"}]
                        }
                    }
                }]);
            } else {
                step["emit"] = serde_json::json!([
                {"kind":"request","id":51,"method":"item/tool/call","params":{
                    "threadId":"${thread_id}","turnId":"${turn_id}","callId":"list-member",
                    "tool":"dolgorae_orchestration","arguments":{"operation":"list_specialists"}
                }},
                {"kind":"request","id":52,"method":"item/tool/call","await_reply":true,"params":{
                    "threadId":"${thread_id}","turnId":"${turn_id}","callId":"assign-member",
                    "tool":"dolgorae_orchestration","arguments":{
                        "operation":"assign_specialist_task",
                        "target":{"run_id":"${specialist_run_id}"},
                        "objective":"Return the fake Codex answer.","context_refs":[],
                        "expected_output":["One result"],"execution_intent":"read_only",
                        "blocking":false,"deadline_seconds":60
                    }
                }},
                {"kind":"request","id":53,"method":"item/tool/call","await_reply":true,"params":{
                    "threadId":"${thread_id}","turnId":"${turn_id}","callId":"await-member",
                    "tool":"dolgorae_orchestration","arguments":{
                        "operation":"await_specialist_tasks","task_ids":["${specialist_task_id}"],
                        "return_when":"all","transport_wait_seconds":10
                    }
                }},
                {"kind":"notification","method":"turn/completed","await_reply":true,"params":{
                    "threadId":"${thread_id}","turn":{
                        "id":"${turn_id}","status":"completed",
                        "items":[{"type":"agentMessage","phase":"final_answer","status":"completed","text":"Production Specialist result"}]
                    }
                }}
                ]);
            }
        }
    }
    let fixture = Fixture::with_scenario(scenario);
    let controller = install_orchestration_controller_with_approval(
        &fixture,
        "production-broker",
        "user_approval_required",
        &fixture.profile,
    );
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let started = runs
        .start_run(pb::StartRunRequest {
            context: context(),
            workspace: Some(fixture.workspace()),
            controller: Some(controller.clone()),
            idempotency_key: "production-broker-start".to_owned(),
            profile_name: fixture.profile.clone(),
            control_mode: pb::ControlMode::DirectInteractive as i32,
            execution_lane: pb::ExecutionLane::SharedReadonly as i32,
            purpose: pb::PurposeKind::Interactive as i32,
            purpose_label: None,
            model: Some("gpt-5.6".to_owned()),
            effort: Some("medium".to_owned()),
            required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
            required_capabilities: Vec::new(),
            instructions: Some("Exercise the production broker adapter.".to_owned()),
            parent: None,
        })
        .await
        .unwrap()
        .into_inner();
    let run = started.run.unwrap();
    let session_id = Uuid::parse_str(&run.run_id).unwrap();
    let run_ref = fixture.run_ref(&run.run_id);
    runs.submit_turn(pb::SubmitTurnRequest {
        context: context(),
        run: run_ref.clone(),
        controller: Some(controller.clone()),
        idempotency_key: "production-broker-warm-primary".to_owned(),
        write_intent: pb::WriteIntent::Read as i32,
        message: "Prepare the Primary Worker.".to_owned(),
        images: Vec::new(),
        effort: None,
        expected_state_revision: run.state_revision,
    })
    .await
    .unwrap();
    native_wait_lifecycle(&mut runs, &fixture, &run.run_id, pb::RunLifecycle::Idle).await;
    assert_eq!(run_count(&fixture), 1);

    let mut interactions = pb::interaction_service_client::InteractionServiceClient::new(channel);
    let pending = interactions
        .list_pending_interactions(pb::ListPendingInteractionsRequest {
            context: context(),
            run: run_ref.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(pending.items.len(), 1);
    let interaction_id = pending.items[0].interaction_id.clone();
    let full = interactions
        .get_controller_interaction(pb::GetControllerInteractionRequest {
            context: context(),
            run: run_ref.clone(),
            controller: Some(controller.clone()),
            interaction_id: interaction_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(full.interaction.unwrap().payload.is_some());
    let resolution = pb::ResolveInteractionRequest {
        context: context(),
        run: run_ref.clone(),
        controller: Some(controller.clone()),
        interaction_id,
        idempotency_key: "production-broker-approval".to_owned(),
        response_json: br#"{"answers":{"specialist_approval":{"answers":["approve"]}}}"#.to_vec(),
    };
    let mut missing_key = resolution.clone();
    missing_key.idempotency_key.clear();
    semantic_error(
        &interactions
            .resolve_interaction(missing_key)
            .await
            .unwrap_err(),
        "INVALID_ARGUMENT",
    );
    let mut oversized_response = resolution.clone();
    oversized_response.response_json = vec![b'x'; 1024 * 1024 + 1];
    semantic_error(
        &interactions
            .resolve_interaction(oversized_response)
            .await
            .unwrap_err(),
        "INTERACTION_RESPONSE_TOO_LARGE",
    );
    assert!(
        OrchestrationStore::open_observer(&fixture.state_root)
            .unwrap()
            .members(session_id)
            .unwrap()
            .is_empty()
    );
    let approved = interactions
        .resolve_interaction(resolution.clone())
        .await
        .unwrap()
        .into_inner();
    let replay = interactions
        .resolve_interaction(resolution)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(approved.resolution_receipt, replay.resolution_receipt);

    let store = OrchestrationStore::open(&fixture.state_root).unwrap();
    let members = store.members(session_id).unwrap();
    assert_eq!(members.len(), 1);
    let child = &members[0];
    assert_eq!(child.membership_state, "active");
    assert_eq!(run_count(&fixture), 2);
    let snapshot =
        dolgorae::snapshot::RunSnapshot::load(&fixture.state_root, child.run_id, 0).unwrap();
    assert!(snapshot.projection.thread_id.is_some());
    assert_eq!(
        snapshot
            .manifest
            .aggregate_binding
            .as_ref()
            .unwrap()
            .aggregate_id,
        session_id
    );
    drop(store);

    let snapshot = native_snapshot(&mut runs, &fixture, &session_id.to_string()).await;
    runs.submit_turn(pb::SubmitTurnRequest {
        context: context(),
        run: fixture.run_ref(&session_id.to_string()),
        controller: Some(controller.clone()),
        idempotency_key: "production-broker-task-turn".to_owned(),
        write_intent: pb::WriteIntent::Read as i32,
        message: "Assign the ready member and await its result.".to_owned(),
        images: Vec::new(),
        effort: None,
        expected_state_revision: snapshot.state_revision,
    })
    .await
    .unwrap();
    native_wait_lifecycle(
        &mut runs,
        &fixture,
        &session_id.to_string(),
        pb::RunLifecycle::Idle,
    )
    .await;
    let results = OrchestrationStore::open(&fixture.state_root)
        .unwrap()
        .collect_results(session_id, 0, 10)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].state, "delivered");
    assert!(results[0].result_artifact_ref.is_some());
    assert!(results[0].result_sha256.is_some());
    let current = native_snapshot(&mut runs, &fixture, &session_id.to_string()).await;
    let closed = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: fixture.run_ref(&session_id.to_string()),
            controller: Some(controller),
            interrupt: false,
            expected_state_revision: current.state_revision,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        closed.run.unwrap().lifecycle,
        pb::RunLifecycle::Closed as i32
    );
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_bootstrap_and_machine_parity() {
    let fixture = Fixture::new("run_start_model_list.json");
    let run_id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runtime = pb::runtime_service_client::RuntimeServiceClient::new(channel.clone());
    let capabilities = runtime
        .get_capabilities(pb::GetCapabilitiesRequest {
            context: context(),
            minimum_protocol_version: 1,
            maximum_protocol_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    let machine = fixture.cli(&["runtime", "capabilities"]);
    let methods: Vec<String> = serde_json::from_value(machine["grpc_methods"].clone()).unwrap();
    let checked: Value = serde_json::from_str(include_str!(
        "../docs/protocol/dolgorae-grpc-conformance-v1.json"
    ))
    .unwrap();
    assert_eq!(capabilities.supported_methods, methods);
    let mut expected = checked["delivery_stages"]["MILESTONE-BH1-P"]["required_methods"]
        .as_array()
        .unwrap()
        .clone();
    expected.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    assert_eq!(
        serde_json::to_value(&methods).unwrap(),
        Value::Array(expected)
    );
    assert_eq!(methods.len(), 27);
    let workspace = runtime
        .inspect_workspace(pb::InspectWorkspaceRequest {
            context: context(),
            absolute_path: fixture.workspace.to_string_lossy().into_owned(),
            expected_workspace_id: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(workspace.workspace_id, fixture.workspace_id);
    assert_eq!(
        workspace.status,
        pb::WorkspaceInspectionStatus::Compatible as i32
    );
    let profile = runtime
        .get_profile(pb::GetProfileRequest {
            context: context(),
            profile_name: fixture.profile.clone(),
        })
        .await
        .unwrap()
        .into_inner()
        .profile
        .unwrap();
    assert_eq!(profile.name, fixture.profile);
    assert_eq!(profile.server_key, fixture.server_key);
    assert_eq!(
        profile
            .models
            .iter()
            .filter(|model| model.is_default)
            .count(),
        1
    );
    let model = profile
        .models
        .iter()
        .find(|model| model.model_id == "gpt-5.6")
        .unwrap();
    assert_eq!(model.supported_efforts, ["high", "low", "medium"]);
    let profiles = runtime
        .list_profiles(pb::ListProfilesRequest { context: context() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(profiles.items.len(), 1);
    assert_eq!(profiles.items[0].models, profile.models);
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let listed = runs
        .list_runs(pb::ListRunsRequest {
            context: context(),
            workspace: Some(fixture.workspace()),
            controller_id: None,
        })
        .await
        .unwrap()
        .into_inner();
    let machine = fixture.cli(&[
        "run",
        "list",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert_eq!(
        listed.items.len(),
        machine["items"].as_array().unwrap().len()
    );
    assert_eq!(listed.items[0].run_id, machine["items"][0]["run_id"]);
    assert_eq!(
        listed.items[0].configuration.as_ref().unwrap().model_id,
        machine["items"][0]["model"]
    );
    for (controller_id, expected_ids) in [
        (fixture.controller_id.clone(), vec![run_id.clone()]),
        (Uuid::now_v7().to_string(), Vec::new()),
    ] {
        let filtered = runs
            .list_runs(pb::ListRunsRequest {
                context: context(),
                workspace: Some(fixture.workspace()),
                controller_id: Some(controller_id),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            filtered
                .items
                .into_iter()
                .map(|run| run.run_id)
                .collect::<Vec<_>>(),
            expected_ids
        );
    }
    semantic_error(
        &runs
            .list_runs(pb::ListRunsRequest {
                context: context(),
                workspace: Some(fixture.workspace()),
                controller_id: Some("not-a-uuid".to_owned()),
            })
            .await
            .unwrap_err(),
        "INVALID_ARGUMENT",
    );
    let mut controllers =
        pb::controller_service_client::ControllerServiceClient::new(channel.clone());
    let verified = controllers
        .verify_controller(pb::VerifyControllerRequest {
            context: context(),
            run: fixture.run_ref(&run_id),
            controller: fixture.carrier(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        verified.controller.unwrap().controller_id,
        fixture.controller_id
    );
    let mut wrong = fixture.carrier().unwrap();
    wrong.expected_controller_generation += 1;
    let rejected = controllers
        .verify_controller(pb::VerifyControllerRequest {
            context: context(),
            run: fixture.run_ref(&run_id),
            controller: Some(wrong),
        })
        .await
        .unwrap_err();
    semantic_error(&rejected, "CONTROLLER_MISMATCH");
    let detail = RichStatus::decode(rejected.details()).unwrap();
    let detail = pb::DolgoraeErrorDetail::decode(detail.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.action, pb::RequiredClientAction::Abort as i32);
    let mut writers = pb::writer_service_client::WriterServiceClient::new(channel);
    let writer = writers
        .get_workspace_writer_status(pb::GetWorkspaceWriterStatusRequest {
            context: context(),
            workspace: Some(fixture.workspace()),
        })
        .await
        .unwrap()
        .into_inner()
        .writer
        .unwrap();
    assert_eq!(
        writer.authority_state,
        pb::WriterAuthorityState::None as i32
    );
    assert_eq!(writer.stamp.unwrap().captured_head_cursor, "");
    let mut unknown = start_request(&fixture, "unknown-enum", pb::ExecutionLane::SharedReadonly);
    unknown.control_mode = 9000;
    semantic_error(
        &runs.start_run(unknown).await.unwrap_err(),
        "UNSUPPORTED_SCHEMA_VERSION",
    );
    let mut unspecified = start_request(
        &fixture,
        "unspecified-control",
        pb::ExecutionLane::SharedReadonly,
    );
    unspecified.control_mode = 0;
    semantic_error(
        &runs.start_run(unspecified).await.unwrap_err(),
        "CONTROL_MODE_REQUIRED",
    );
    let unavailable = runs
        .set_default_effort(pb::SetDefaultEffortRequest {
            context: context(),
            run: fixture.run_ref(&run_id),
            controller: fixture.carrier(),
            effort: "high".into(),
            expected_state_revision: listed.items[0].state_revision,
        })
        .await
        .unwrap_err();
    semantic_error(&unavailable, "CAPABILITY_UNSUPPORTED");
    assert_eq!(run_count(&fixture), 1, "rejected inputs allocated a Run");

    let image_path = fixture.workspace.join("image.png");
    let absolute_image = image_path.to_str().unwrap();
    let image = |path: &str, detail| {
        vec![pb::ImageInput {
            absolute_file_path: path.to_owned(),
            detail,
        }]
    };
    let before = audit(&fixture, &run_id);
    for (index, (write_intent, images, error)) in [
        (0, Vec::new(), "INVALID_ARGUMENT"),
        (9000, Vec::new(), "UNSUPPORTED_SCHEMA_VERSION"),
        (
            pb::WriteIntent::Read as i32,
            image("image.png", pb::ImageDetail::High as i32),
            "INVALID_ARGUMENT",
        ),
        (
            pb::WriteIntent::Read as i32,
            image(
                fixture.workspace.join("../image.png").to_str().unwrap(),
                pb::ImageDetail::High as i32,
            ),
            "INVALID_ARGUMENT",
        ),
        (
            pb::WriteIntent::Read as i32,
            image(absolute_image, 0),
            "INVALID_ARGUMENT",
        ),
        (
            pb::WriteIntent::Read as i32,
            image(absolute_image, 9000),
            "UNSUPPORTED_SCHEMA_VERSION",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        semantic_error(
            &runs
                .submit_turn(pb::SubmitTurnRequest {
                    context: context(),
                    run: fixture.run_ref(&run_id),
                    controller: fixture.carrier(),
                    idempotency_key: format!("invalid-image-{index}"),
                    write_intent,
                    message: "Decode before admitting a Turn.".to_owned(),
                    images,
                    effort: None,
                    expected_state_revision: listed.items[0].state_revision,
                })
                .await
                .unwrap_err(),
            error,
        );
        assert_eq!(
            audit(&fixture, &run_id),
            before,
            "invalid SubmitTurn changed the audit"
        );
    }
    assert!(transcript_methods(&fixture, "turn/start").is_empty());
    use base64::Engine as _;
    fs::write(&image_path, base64::engine::general_purpose::STANDARD.decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jU1sAAAAASUVORK5CYII="
    ).unwrap()).unwrap();
    let current = native_snapshot(&mut runs, &fixture, &run_id).await;
    let accepted = runs
        .submit_turn(pb::SubmitTurnRequest {
            context: context(),
            run: fixture.run_ref(&run_id),
            controller: fixture.carrier(),
            idempotency_key: "valid-image".to_owned(),
            write_intent: pb::WriteIntent::Read as i32,
            message: "Inspect the supplied image.".to_owned(),
            images: image(absolute_image, pb::ImageDetail::High as i32),
            effort: None,
            expected_state_revision: current.state_revision,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(accepted.accepted_turn.unwrap().turn_id, "turn-1");
    native_wait_lifecycle(&mut runs, &fixture, &run_id, pb::RunLifecycle::Idle).await;
    let sent = transcript_methods(&fixture, "turn/start");
    assert_eq!(sent.len(), 1);
    let forwarded = sent[0]["params"]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|input| input["type"] == "localImage")
        .unwrap();
    assert_eq!(forwarded["path"], absolute_image);
    assert_eq!(forwarded["detail"], "high");
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orchestration_queries_reject_a_low_level_run_after_controller_authentication() {
    let fixture = Fixture::new("run_start_model_list.json");
    let run_id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut orchestration =
        pb::orchestration_service_client::OrchestrationServiceClient::new(channel.clone());
    let status = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: fixture.run_ref(&run_id),
            controller: fixture.carrier(),
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(
        semantic_error(&status, "INVALID_ARGUMENT").action,
        pb::RequiredClientAction::FixRequest as i32
    );
    let status = orchestration
        .list_orchestrated_session_results(pb::ListOrchestratedSessionResultsRequest {
            context: context(),
            root_run: fixture.run_ref(&run_id),
            controller: fixture.carrier(),
            page_cursor: None,
            limit: 1,
            projection_version: 1,
        })
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::InvalidArgument);
    semantic_error(&status, "INVALID_ARGUMENT");
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_reconcile_rejects_a_healthy_run_without_effect() {
    let fixture = Fixture::new("run_start_model_list.json");
    let run_id = fixture.start_run(&[]);
    let before = audit(&fixture, &run_id);
    let mut gateway = fixture.start_gateway();
    let mut runs = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let current = native_snapshot(&mut runs, &fixture, &run_id).await;
    let rejected = runs
        .reconcile_run(pb::ReconcileRunRequest {
            context: context(),
            run: fixture.run_ref(&run_id),
            controller: fixture.carrier(),
            expected_state_revision: current.state_revision,
        })
        .await
        .unwrap_err();
    assert_eq!(rejected.code(), Code::Aborted);
    semantic_error(&rejected, "RUN_STATE_CONFLICT");
    assert_eq!(audit(&fixture, &run_id), before);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_close_persists_one_operation_and_survives_gateway_restart() {
    let fixture = Fixture::new("run_start_model_list.json");
    let controller = install_orchestration_controller(&fixture, "session-retirement");
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let started = runs
        .start_run(pb::StartRunRequest {
            context: context(),
            workspace: Some(fixture.workspace()),
            controller: Some(controller.clone()),
            idempotency_key: Uuid::now_v7().to_string(),
            profile_name: fixture.profile.clone(),
            control_mode: pb::ControlMode::DirectInteractive as i32,
            execution_lane: pb::ExecutionLane::SharedReadonly as i32,
            purpose: pb::PurposeKind::Interactive as i32,
            purpose_label: None,
            model: Some("gpt-5.6".to_owned()),
            effort: Some("medium".to_owned()),
            required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
            required_capabilities: Vec::new(),
            instructions: Some("Exercise durable whole-session retirement.".to_owned()),
            parent: None,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    let run = pb::RunRef {
        workspace: Some(fixture.workspace()),
        run_id: started.run_id.clone(),
    };
    let close_request = pb::CloseRunRequest {
        context: context(),
        run: Some(run.clone()),
        controller: Some(controller.clone()),
        interrupt: false,
        expected_state_revision: started.state_revision,
    };
    let mut concurrent_runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let (first_close, second_close) = tokio::join!(
        runs.close_run(close_request.clone()),
        concurrent_runs.close_run(close_request)
    );
    let (closed, concurrent) = match (first_close, second_close) {
        (Ok(closed), concurrent) => (closed.into_inner(), concurrent),
        (Err(first), Ok(closed)) => {
            assert_eq!(first.code(), Code::FailedPrecondition, "{first:?}");
            (closed.into_inner(), Err(first))
        }
        (Err(first), Err(second)) => panic!("both compatible closes failed: {first}; {second}"),
    };
    let operation_id = closed.context.unwrap().operation_id.unwrap();
    match concurrent {
        Ok(concurrent) => assert_eq!(
            concurrent
                .into_inner()
                .context
                .unwrap()
                .operation_id
                .as_deref(),
            Some(operation_id.as_str())
        ),
        Err(status) => {
            assert_eq!(status.code(), Code::FailedPrecondition, "{status:?}");
            let detail = semantic_error(&status, "SESSION_CLOSE_IN_PROGRESS");
            assert_eq!(detail.operation_id.as_deref(), Some(operation_id.as_str()));
        }
    }
    let closed_run = closed.run.unwrap();
    assert_eq!(closed_run.lifecycle, pb::RunLifecycle::Closed as i32);

    let mut orchestration =
        pb::orchestration_service_client::OrchestrationServiceClient::new(channel);
    let observed = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: Some(controller.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert_eq!(
        observed.lifecycle,
        pb::OrchestratedSessionLifecycle::Completed as i32
    );
    assert_eq!(
        observed.close_progress,
        pb::SessionCloseProgress::Completed as i32
    );
    assert_eq!(
        observed.close_operation_id.as_deref(),
        Some(operation_id.as_str())
    );

    gateway.terminate();
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut orchestration =
        pb::orchestration_service_client::OrchestrationServiceClient::new(channel.clone());
    let restarted = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: Some(controller.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert_eq!(restarted.close_operation_id, Some(operation_id.clone()));
    assert_eq!(restarted.aggregate_revision, observed.aggregate_revision);

    let mut runs = pb::run_service_client::RunServiceClient::new(channel);
    let replay = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: Some(run.clone()),
            controller: Some(controller.clone()),
            interrupt: false,
            expected_state_revision: closed_run.state_revision,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        replay.context.unwrap().operation_id.as_deref(),
        Some(operation_id.as_str())
    );
    let conflict = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: Some(run),
            controller: Some(controller),
            interrupt: true,
            expected_state_revision: replay.run.unwrap().state_revision,
        })
        .await
        .unwrap_err();
    assert_eq!(conflict.code(), Code::Aborted);
    semantic_error(&conflict, "RUN_STATE_CONFLICT");
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_recovery_resumes_a_close_intent_committed_before_gateway_restart() {
    let fixture = Fixture::new("run_start_model_list.json");
    let controller = install_orchestration_controller(&fixture, "session-close-recovery");
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel);
    let started = runs
        .start_run(pb::StartRunRequest {
            context: context(),
            workspace: Some(fixture.workspace()),
            controller: Some(controller.clone()),
            idempotency_key: Uuid::now_v7().to_string(),
            profile_name: fixture.profile.clone(),
            control_mode: pb::ControlMode::DirectInteractive as i32,
            execution_lane: pb::ExecutionLane::SharedReadonly as i32,
            purpose: pb::PurposeKind::Interactive as i32,
            purpose_label: None,
            model: Some("gpt-5.6".to_owned()),
            effort: Some("medium".to_owned()),
            required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
            required_capabilities: Vec::new(),
            instructions: Some("Recover a retained close intent without new work.".to_owned()),
            parent: None,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    let run_id = Uuid::parse_str(&started.run_id).unwrap();
    let run = pb::RunRef {
        workspace: Some(fixture.workspace()),
        run_id: started.run_id.clone(),
    };
    gateway.terminate();

    let mut store = OrchestrationStore::open(&fixture.state_root).unwrap();
    let (close, admitted) = store.begin_session_close(run_id, false, 1).unwrap();
    assert!(admitted);
    let operation_id = close.unwrap().operation_id.to_string();
    drop(store);

    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut orchestration =
        pb::orchestration_service_client::OrchestrationServiceClient::new(channel.clone());
    let interrupted = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: Some(controller.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert_eq!(
        interrupted.lifecycle,
        pb::OrchestratedSessionLifecycle::Completing as i32
    );
    assert_eq!(
        interrupted.close_operation_id.as_deref(),
        Some(operation_id.as_str())
    );

    let mut runs = pb::run_service_client::RunServiceClient::new(channel);
    let recovered = runs
        .recover_run(pb::RecoverRunRequest {
            context: context(),
            run: Some(run.clone()),
            controller: Some(controller.clone()),
            expected_state_revision: started.state_revision,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        recovered.context.unwrap().operation_id.as_deref(),
        Some(operation_id.as_str())
    );
    assert_eq!(
        recovered.run.unwrap().lifecycle,
        pb::RunLifecycle::Closed as i32
    );
    let completed = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: Some(run),
            controller: Some(controller),
        })
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert_eq!(
        completed.close_progress,
        pb::SessionCloseProgress::Completed as i32
    );
    assert_eq!(completed.close_operation_id, Some(operation_id));
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_close_requires_explicit_interrupt_before_committing_active_work() {
    let fixture = Fixture::with_scenario(active_turn_scenario(false));
    let controller = install_orchestration_controller(&fixture, "session-active-close");
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let started = runs
        .start_run(pb::StartRunRequest {
            context: context(),
            workspace: Some(fixture.workspace()),
            controller: Some(controller.clone()),
            idempotency_key: Uuid::now_v7().to_string(),
            profile_name: fixture.profile.clone(),
            control_mode: pb::ControlMode::DirectInteractive as i32,
            execution_lane: pb::ExecutionLane::SharedReadonly as i32,
            purpose: pb::PurposeKind::Interactive as i32,
            purpose_label: None,
            model: Some("gpt-5.6".to_owned()),
            effort: Some("medium".to_owned()),
            required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
            required_capabilities: Vec::new(),
            instructions: Some("Require explicit interruption before retirement.".to_owned()),
            parent: None,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    let run = pb::RunRef {
        workspace: Some(fixture.workspace()),
        run_id: started.run_id.clone(),
    };
    runs.submit_turn(pb::SubmitTurnRequest {
        context: context(),
        run: Some(run.clone()),
        controller: Some(controller.clone()),
        idempotency_key: "active-before-root-close".to_owned(),
        write_intent: pb::WriteIntent::Read as i32,
        message: "Remain active until explicitly interrupted.".to_owned(),
        images: Vec::new(),
        effort: None,
        expected_state_revision: started.state_revision,
    })
    .await
    .unwrap();
    let active = native_wait_lifecycle(
        &mut runs,
        &fixture,
        &started.run_id,
        pb::RunLifecycle::Running,
    )
    .await;
    let rejected = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: Some(run.clone()),
            controller: Some(controller.clone()),
            interrupt: false,
            expected_state_revision: active.state_revision,
        })
        .await
        .unwrap_err();
    semantic_error(&rejected, "RUN_STATE_CONFLICT");
    let mut orchestration =
        pb::orchestration_service_client::OrchestrationServiceClient::new(channel);
    let before_interrupt = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: Some(controller.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert!(before_interrupt.close_operation_id.is_none());
    assert_eq!(
        before_interrupt.close_progress,
        pb::SessionCloseProgress::None as i32
    );

    let closed = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: Some(run),
            controller: Some(controller),
            interrupt: true,
            expected_state_revision: active.state_revision,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(closed.context.unwrap().operation_id.is_some());
    assert_eq!(
        closed.run.unwrap().lifecycle,
        pb::RunLifecycle::Closed as i32
    );
    assert_eq!(transcript_methods(&fixture, "turn/interrupt").len(), 1);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_orchestrated_session_observation_survives_gateway_restart_without_mutation() {
    let fixture = Fixture::new("run_start_model_list.json");
    let roles = fixture.workspace.join(".dolgorae/roles");
    fs::create_dir_all(&roles).unwrap();
    fs::write(
        roles.join("observer.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "name": "observer",
            "display_name": "Observer",
            "description": "Returns bounded observations.",
            "instructions": "Return a bounded observation without modifying the workspace."
        }))
        .unwrap(),
    )
    .unwrap();
    let policy = fixture.root.join("session-observe-policy.json");
    fs::write(
        &policy,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "policy_name": "session-observe",
            "revision": 7,
            "approval_policy": "fully_delegated",
            "max_active_specialists": 1,
            "roles": [{
                "role_ref": "observer",
                "role_source": {"scope":"project","name":"observer"},
                "agent_configuration": {
                    "schema_version": 2,
                    "selected_profile": fixture.profile,
                    "global_profile_binding_sha256": null,
                    "model": "gpt-5.6",
                    "default_effort": "medium",
                    "purpose": "review",
                    "purpose_label": null,
                    "required_capabilities": [],
                    "execution_lane": "dedicated",
                    "required_assurance": "best_effort_personal_alpha",
                    "native_subagent_policy": "enabled"
                },
                "max_active_instances": 1,
                "reuse_policy": "never",
                "allowed_access": ["read_only"],
                "activation_policy": "keep_resident",
                "primary_may_request": true,
                "collaboration_source": false,
                "collaboration_target": false,
                "auto_approve_when_fully_delegated": true
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    fixture.cli(&[
        "specialist",
        "policy",
        "add",
        "session-observe",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--file",
        policy.to_str().unwrap(),
    ]);
    let controller_path = fixture
        .home
        .join(".dolgorae/controller-carriers/gateway-native/test-installation/orchestrator.json");
    let created = fixture.cli(&[
        "controller",
        "credential",
        "create",
        "--kind",
        "interactive-client",
        "--instance-id",
        "native-orchestrator",
        "--orchestration-policy",
        "session-observe",
        "--output",
        controller_path.to_str().unwrap(),
    ]);
    let controller_id = created["controller"]["controller_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let orchestrator = Some(pb::ControllerCarrierRef {
        absolute_file_path: controller_path.to_string_lossy().into_owned(),
        expected_controller_id: controller_id,
        expected_controller_generation: 1,
    });
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let started = runs
        .start_run(pb::StartRunRequest {
            context: context(),
            workspace: Some(fixture.workspace()),
            controller: orchestrator.clone(),
            idempotency_key: Uuid::now_v7().to_string(),
            profile_name: fixture.profile.clone(),
            control_mode: pb::ControlMode::DirectInteractive as i32,
            execution_lane: pb::ExecutionLane::SharedReadonly as i32,
            purpose: pb::PurposeKind::Interactive as i32,
            purpose_label: None,
            model: Some("gpt-5.6".to_owned()),
            effort: Some("medium".to_owned()),
            required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
            required_capabilities: Vec::new(),
            instructions: Some("Exercise public Orchestrated Session observation.".to_owned()),
            parent: None,
        })
        .await
        .unwrap()
        .into_inner();
    let run = pb::RunRef {
        workspace: Some(fixture.workspace()),
        run_id: started.run.unwrap().run_id,
    };
    let request = pb::GetOrchestratedSessionRequest {
        context: context(),
        root_run: Some(run.clone()),
        controller: orchestrator.clone(),
    };
    let mut orchestration =
        pb::orchestration_service_client::OrchestrationServiceClient::new(channel.clone());
    let first = orchestration
        .get_orchestrated_session(request.clone())
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert_eq!(first.session_id, run.run_id);
    assert_eq!(
        first.lifecycle,
        pb::OrchestratedSessionLifecycle::Active as i32
    );
    assert_eq!(
        first.composition,
        pb::OrchestratedSessionComposition::StandalonePrimary as i32
    );
    assert_eq!(first.specialist_policy_name, "session-observe");
    assert_eq!(first.specialist_policy_revision, 7);
    assert_eq!(first.nonretired_member_count, 1);
    assert_eq!(first.published_result_count, 0);
    let denied = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: fixture.carrier(),
        })
        .await
        .unwrap_err();
    assert_eq!(denied.code(), Code::PermissionDenied);
    semantic_error(&denied, "CONTROLLER_MISMATCH");
    let empty = orchestration
        .list_orchestrated_session_results(pb::ListOrchestratedSessionResultsRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: orchestrator.clone(),
            page_cursor: None,
            limit: 0,
            projection_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(empty.captured_publication_head, 0);
    assert!(empty.items.is_empty());
    assert!(empty.next_page_cursor.is_none());

    let session_id = Uuid::parse_str(&run.run_id).unwrap();
    let mut store = OrchestrationStore::open(&fixture.state_root).unwrap();
    let mut adapter = PublicResultAdapter;
    let call = |key: &str| PrimaryCallContext {
        session_id,
        source_run_id: session_id,
        source_turn_id: "primary-turn".to_owned(),
        source_tool_call_id: format!("tool-{key}"),
        idempotency_key: key.to_owned(),
    };
    let specialist = store
        .request_specialist(
            &call("request"),
            &RequestSpecialist {
                role_ref: "observer".to_owned(),
                objective: "Produce one public result.".to_owned(),
                expected_output: vec!["One bounded result".to_owned()],
                requested_access: "read_only".to_owned(),
                deadline_seconds: 60,
            },
            &mut adapter,
        )
        .unwrap();
    let specialist_run_id = specialist.specialist_run_id.unwrap();
    let task = store
        .assign_task(
            &call("assign"),
            &AssignSpecialistTask {
                specialist_run_id,
                objective: "Return the public result bytes.".to_owned(),
                context_refs: Vec::new(),
                expected_output: vec!["UTF-8 result".to_owned()],
                requested_access: "read_only".to_owned(),
                deadline_seconds: 60,
            },
            &mut adapter,
        )
        .unwrap();
    let second_task = store
        .assign_task(
            &call("assign-two"),
            &AssignSpecialistTask {
                specialist_run_id,
                objective: "Return the second public result bytes.".to_owned(),
                context_refs: Vec::new(),
                expected_output: vec!["Second UTF-8 result".to_owned()],
                requested_access: "read_only".to_owned(),
                deadline_seconds: 60,
            },
            &mut adapter,
        )
        .unwrap();
    assert_eq!(task.state, "completed_not_delivered");
    assert_eq!(second_task.state, "completed_not_delivered");
    assert_eq!(store.collect_results(session_id, 0, 8).unwrap().len(), 2);
    drop(store);

    let published = orchestration
        .list_orchestrated_session_results(pb::ListOrchestratedSessionResultsRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: orchestrator.clone(),
            page_cursor: None,
            limit: 1,
            projection_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(published.items.len(), 1);
    let fixed_head_cursor = published.next_page_cursor.clone().unwrap();
    let mut malformed_cursor = fixed_head_cursor.clone().into_bytes();
    malformed_cursor[0] = if malformed_cursor[0] == b'A' {
        b'B'
    } else {
        b'A'
    };
    let malformed = orchestration
        .list_orchestrated_session_results(pb::ListOrchestratedSessionResultsRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: orchestrator.clone(),
            page_cursor: Some(String::from_utf8(malformed_cursor).unwrap()),
            limit: 1,
            projection_version: 1,
        })
        .await
        .unwrap_err();
    assert_eq!(malformed.code(), Code::InvalidArgument);
    semantic_error(&malformed, "INVALID_ARGUMENT");
    let result = &published.items[0];
    assert_eq!(result.task_id, task.task_id.to_string());
    assert_eq!(
        result.artifact_owner.as_ref().unwrap().run_id,
        session_id.to_string()
    );
    let artifact = result.artifact.as_ref().unwrap();
    assert_eq!(
        artifact.visibility,
        pb::ArtifactVisibility::ControllerOnly as i32
    );
    let mut artifacts = pb::artifact_service_client::ArtifactServiceClient::new(channel);
    let metadata = artifacts
        .get_artifact(pb::GetArtifactRequest {
            context: context(),
            run: Some(run.clone()),
            artifact_id: artifact.artifact_id.clone(),
            controller: orchestrator.clone(),
        })
        .await
        .unwrap()
        .into_inner()
        .artifact
        .unwrap();
    assert_eq!(metadata.byte_length, result.byte_length);
    assert_eq!(metadata.sha256, result.sha256);
    let chunk = artifacts
        .read_artifact_chunk(pb::ReadArtifactChunkRequest {
            context: context(),
            run: Some(run.clone()),
            artifact_id: artifact.artifact_id.clone(),
            offset: 0,
            length: metadata.byte_length as u32,
            controller: orchestrator.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(chunk.sha256, result.sha256);
    assert_eq!(chunk.data.len() as u64, result.byte_length);

    let mut store = OrchestrationStore::open(&fixture.state_root).unwrap();
    let third_task = store
        .assign_task(
            &call("assign-three"),
            &AssignSpecialistTask {
                specialist_run_id,
                objective: "Return a result published after page one.".to_owned(),
                context_refs: Vec::new(),
                expected_output: vec!["Third UTF-8 result".to_owned()],
                requested_access: "read_only".to_owned(),
                deadline_seconds: 60,
            },
            &mut adapter,
        )
        .unwrap();
    assert_eq!(third_task.state, "completed_not_delivered");
    assert_eq!(store.collect_results(session_id, 0, 8).unwrap().len(), 3);
    store
        .release_specialist(session_id, specialist_run_id, &mut adapter)
        .unwrap();
    drop(store);
    let fixed_second_page = orchestration
        .list_orchestrated_session_results(pb::ListOrchestratedSessionResultsRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: orchestrator.clone(),
            page_cursor: Some(fixed_head_cursor),
            limit: 1,
            projection_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fixed_second_page.items.len(), 1);
    assert_eq!(
        fixed_second_page.items[0].task_id,
        second_task.task_id.to_string()
    );
    assert!(fixed_second_page.next_page_cursor.is_none());
    assert_eq!(
        fixed_second_page.captured_publication_head,
        published.captured_publication_head
    );
    let fresh_results = orchestration
        .list_orchestrated_session_results(pb::ListOrchestratedSessionResultsRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: orchestrator.clone(),
            page_cursor: None,
            limit: 500,
            projection_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fresh_results.items.len(), 3);
    assert!(fresh_results.captured_publication_head > published.captured_publication_head);
    let after_publication = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: orchestrator.clone(),
        })
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert_eq!(after_publication.published_result_count, 3);
    assert_eq!(after_publication.accepted_unfinished_task_count, 0);
    assert!(after_publication.aggregate_revision > first.aggregate_revision);
    gateway.terminate();

    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut orchestration =
        pb::orchestration_service_client::OrchestrationServiceClient::new(channel);
    let restarted = orchestration
        .get_orchestrated_session(pb::GetOrchestratedSessionRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: orchestrator.clone(),
        })
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap();
    assert_eq!(
        restarted.aggregate_revision,
        after_publication.aggregate_revision
    );
    assert_eq!(restarted.source_revision, after_publication.source_revision);
    let retained = orchestration
        .list_orchestrated_session_results(pb::ListOrchestratedSessionResultsRequest {
            context: context(),
            root_run: Some(run.clone()),
            controller: orchestrator.clone(),
            page_cursor: None,
            limit: 1,
            projection_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retained.items.len(), 1);
    assert_eq!(
        retained.captured_publication_head,
        fresh_results.captured_publication_head
    );
    let refused = fixture.command(&[
        "run",
        "--controller-file",
        controller_path.to_str().unwrap(),
        "close",
        &run.run_id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    let refused_json: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(refused_json["error"]["code"], "RUN_STATE_CONFLICT");
    let current = native_snapshot(&mut runs, &fixture, &run.run_id).await;
    let closed = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: Some(run.clone()),
            controller: orchestrator,
            interrupt: false,
            expected_state_revision: current.state_revision,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        closed.run.unwrap().lifecycle,
        pb::RunLifecycle::Closed as i32
    );
    gateway.terminate();
}

fn audit(fixture: &Fixture, run_id: &str) -> Vec<u8> {
    fs::read(
        fixture
            .state_root
            .join("runs")
            .join(run_id)
            .join("audit.jsonl"),
    )
    .unwrap()
}

fn audit_summary(fixture: &Fixture, run_id: &str) -> Value {
    Value::Array(audit(fixture, run_id).split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty()).map(|line| {
            let record: Value = serde_json::from_slice(line).unwrap();
            serde_json::json!({"sequence":record["sequence"],"kind":record["kind"],
                "payload": if matches!(record["kind"].as_str(), Some("mutation_admitted" | "reconciliation" | "outcome_unknown")) {
                    record["payload"].clone()
                } else { Value::Null }})
        }).collect())
}

fn turn_starts(fixture: &Fixture, run_id: &str) -> usize {
    audit(fixture, run_id)
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .filter(|record| record["kind"] == "turn_started")
        .count()
}

fn upstream_turn_starts(fixture: &Fixture) -> usize {
    fs::read(fixture.root.join("transcript.jsonl"))
        .unwrap()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .filter(|message| message["method"] == "turn/start")
        .count()
}

fn concurrent_admission_result<T>(
    result: Result<tonic::Response<T>, tonic::Status>,
    operation: &str,
) -> Option<T> {
    match result {
        Ok(response) => Some(response.into_inner()),
        Err(status) => {
            let rich = RichStatus::decode(status.details()).unwrap();
            let detail = pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap();
            assert!(
                matches!(
                    detail.dolgorae_error_code.as_str(),
                    "RUN_STATE_CONFLICT" | "RUN_BUSY"
                ),
                "unexpected concurrent {operation} admission failure: {status}"
            );
            semantic_error(&status, &detail.dolgorae_error_code);
            None
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_concurrent_identical_admission() {
    let fixture = Fixture::new("run_start_model_list.json");
    let mut gateway = fixture.start_gateway();
    let mut first = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let mut second = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let request = start_request(
        &fixture,
        "concurrent-start",
        pb::ExecutionLane::SharedReadonly,
    );
    let (left, right) = tokio::time::timeout(Duration::from_secs(90), async {
        tokio::join!(
            first.start_run(request.clone()),
            second.start_run(request.clone())
        )
    })
    .await
    .expect("concurrent StartRun admission did not complete");
    let allocated: Vec<_> = [left, right]
        .into_iter()
        .filter_map(|result| concurrent_admission_result(result, "StartRun"))
        .collect();
    assert!(
        !allocated.is_empty(),
        "neither concurrent allocation was accepted"
    );
    assert_eq!(
        allocated
            .iter()
            .filter(|result| !result.exact_replay)
            .count(),
        1
    );
    let replay = first.start_run(request).await.unwrap().into_inner();
    assert!(replay.exact_replay);
    let run = replay.run.as_ref().unwrap();
    for result in allocated {
        assert_eq!(result.idempotency_key, replay.idempotency_key);
        assert_eq!(result.run.as_ref().unwrap().run_id, run.run_id);
    }
    assert_eq!(run_count(&fixture), 1);
    let membership: Vec<Value> = fs::read_to_string(
        fixture
            .home
            .join(".dolgorae/profiles")
            .join(&fixture.server_key)
            .join("membership.jsonl"),
    )
    .unwrap()
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .filter(|record: &Value| record["run_id"] == run.run_id)
    .collect();
    assert_eq!(
        membership
            .iter()
            .filter(|record| record["lifecycle"] == "admitting")
            .count(),
        1
    );
    assert!(
        membership
            .iter()
            .all(|record| record["disposition"] != "released")
    );
    assert_eq!(membership.last().unwrap()["disposition"], "active");
    assert_eq!(transcript_methods(&fixture, "thread/start").len(), 0);

    let request = pb::SubmitTurnRequest {
        context: context(),
        run: fixture.run_ref(&run.run_id),
        controller: fixture.carrier(),
        idempotency_key: "concurrent-turn".into(),
        write_intent: pb::WriteIntent::Read as i32,
        message: "Accept exactly one upstream Turn.".into(),
        images: vec![],
        effort: None,
        expected_state_revision: run.state_revision,
    };
    let (left, right) = tokio::time::timeout(Duration::from_secs(90), async {
        tokio::join!(
            first.submit_turn(request.clone()),
            second.submit_turn(request.clone())
        )
    })
    .await
    .expect("concurrent SubmitTurn admission did not complete");
    let accepted: Vec<_> = [left, right]
        .into_iter()
        .filter_map(|result| concurrent_admission_result(result, "SubmitTurn"))
        .collect();
    assert!(
        !accepted.is_empty(),
        "neither concurrent submission was accepted"
    );
    native_wait_lifecycle(&mut first, &fixture, &run.run_id, pb::RunLifecycle::Idle).await;
    // The original revision is deliberately retained: accepted replay precedes
    // the new-operation revision check, even after terminal publication.
    let replay = first.submit_turn(request).await.unwrap().into_inner();
    assert_eq!(
        replay.accepted_turn.as_ref().unwrap().status,
        pb::TurnStatus::Accepted as i32
    );
    for result in accepted {
        assert_eq!(result.accepted_turn, replay.accepted_turn);
        assert_eq!(result.run, replay.run);
        assert_eq!(
            result.writer.as_ref().unwrap().stamp,
            replay.writer.as_ref().unwrap().stamp
        );
        assert_eq!(result.correlation_id, replay.correlation_id);
        assert_eq!(
            result.context.as_ref().unwrap().operation_id,
            replay.context.as_ref().unwrap().operation_id
        );
    }
    assert_eq!(run_count(&fixture), 1);
    assert_eq!(turn_starts(&fixture, &run.run_id), 1);
    assert_eq!(upstream_turn_starts(&fixture), 1);
    assert_eq!(transcript_methods(&fixture, "thread/start").len(), 1);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_resume_accepts_next_turn() {
    let fixture = Fixture::new("run_start_model_list.json");
    let id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let mut runs = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let initial = native_snapshot(&mut runs, &fixture, &id).await;
    native_submit(&mut runs, &fixture, &initial, pb::WriteIntent::Read).await;
    let idle = native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Idle).await;
    let paused = runs
        .pause_run(pb::PauseRunRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            interrupt: false,
            expected_state_revision: idle.state_revision,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert_eq!(paused.lifecycle, pb::RunLifecycle::Paused as i32);
    let resumed = runs
        .resume_run(pb::ResumeRunRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            expected_state_revision: paused.state_revision,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert_eq!(resumed.lifecycle, pb::RunLifecycle::Idle as i32);
    assert!(resumed.state_revision > paused.state_revision);
    assert!(resumed.active_turn.is_none());
    assert_eq!(upstream_turn_starts(&fixture), 1, "resume submitted a Turn");
    let accepted = native_submit(&mut runs, &fixture, &resumed, pb::WriteIntent::Read).await;
    assert_eq!(
        accepted.accepted_turn.unwrap().status,
        pb::TurnStatus::Accepted as i32
    );
    native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Idle).await;
    assert_eq!(turn_starts(&fixture, &id), 2);
    assert_eq!(upstream_turn_starts(&fixture), 2);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_context_and_unavailable_method_rejection() {
    let fixture = Fixture::new("run_start_model_list.json");
    let id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let mut runs = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let mut observations =
        pb::observation_service_client::ObservationServiceClient::new(gateway.channel().await);
    let before = audit(&fixture, &id);
    let mut invalid_uuid = context().unwrap();
    invalid_uuid.client_request_id = "not-a-uuid".into();
    let mut unnegotiated = context().unwrap();
    unnegotiated.protocol_version = 0;
    let mut unsupported = context().unwrap();
    unsupported.protocol_version = 2;
    for (context, expected) in [
        (None, "INVALID_ARGUMENT"),
        (Some(invalid_uuid), "INVALID_ARGUMENT"),
        (Some(unnegotiated), "PROTOCOL_VERSION_UNSUPPORTED"),
        (Some(unsupported), "PROTOCOL_VERSION_UNSUPPORTED"),
    ] {
        let mut request = start_request(
            &fixture,
            "invalid-context",
            pb::ExecutionLane::SharedReadonly,
        );
        request.context = context.clone();
        semantic_error(&runs.start_run(request).await.unwrap_err(), expected);
        let watch = observations
            .watch_run_events(pb::WatchRunEventsRequest {
                context: context.clone(),
                ..Default::default()
            })
            .await;
        let status = match watch {
            Err(status) => status,
            Ok(response) => response.into_inner().message().await.unwrap_err(),
        };
        semantic_error(&status, expected);
        // Context validation also precedes the capability-unavailable response.
        semantic_error(
            &runs
                .delete_run(pb::DeleteRunRequest {
                    context,
                    run: fixture.run_ref(&id),
                    controller: fixture.carrier(),
                    confirm: true,
                    expected_state_revision: 0,
                })
                .await
                .unwrap_err(),
            expected,
        );
    }
    // These methods are declared on the wire but excluded from this stage's
    // supported-method set. Valid contexts must fail without backend effects.
    semantic_error(
        &runs
            .fork_run(pb::ForkRunRequest {
                context: context(),
                ..Default::default()
            })
            .await
            .unwrap_err(),
        "CAPABILITY_UNSUPPORTED",
    );
    let mut writers = pb::writer_service_client::WriterServiceClient::new(gateway.channel().await);
    semantic_error(
        &writers
            .prepare_writer_handoff(pb::PrepareWriterHandoffRequest {
                context: context(),
                ..Default::default()
            })
            .await
            .unwrap_err(),
        "CAPABILITY_UNSUPPORTED",
    );
    semantic_error(
        &writers
            .commit_writer_handoff(pb::CommitWriterHandoffRequest {
                context: context(),
                ..Default::default()
            })
            .await
            .unwrap_err(),
        "CAPABILITY_UNSUPPORTED",
    );
    semantic_error(
        &writers
            .cancel_writer_handoff(pb::CancelWriterHandoffRequest {
                context: context(),
                ..Default::default()
            })
            .await
            .unwrap_err(),
        "CAPABILITY_UNSUPPORTED",
    );
    assert_eq!(audit(&fixture, &id), before);
    assert_eq!(run_count(&fixture), 1);
    assert_eq!(upstream_turn_starts(&fixture), 0);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controller_timeline_preserves_input_and_pages_without_replay_duplicates() {
    let fixture = Fixture::with_scenario(timeline_scenario());
    let id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut observations =
        pb::observation_service_client::ObservationServiceClient::new(channel.clone());
    let initial = native_snapshot(&mut runs, &fixture, &id).await;
    let image_path = fixture.workspace.join("timeline.png");
    use base64::Engine as _;
    fs::write(
        &image_path,
        base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jU1sAAAAASUVORK5CYII=")
            .unwrap(),
    )
    .unwrap();
    let message = "첫 줄 🐋\r\n둘째 줄\n";
    let request = pb::SubmitTurnRequest {
        context: context(),
        run: fixture.run_ref(&id),
        controller: fixture.carrier(),
        idempotency_key: "timeline-original".to_owned(),
        write_intent: pb::WriteIntent::Read as i32,
        message: message.to_owned(),
        images: vec![pb::ImageInput {
            absolute_file_path: image_path.to_string_lossy().into_owned(),
            detail: pb::ImageDetail::High as i32,
        }],
        effort: None,
        expected_state_revision: initial.state_revision,
    };
    let accepted = runs
        .submit_turn(request.clone())
        .await
        .unwrap()
        .into_inner();
    native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Idle).await;

    let unauthorized = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: None,
            after_cursor: "0".to_owned(),
            limit: 100,
            timeline_version: 1,
        })
        .await
        .unwrap_err();
    semantic_error(
        &unauthorized,
        "INTERACTION_FULL_PAYLOAD_REQUIRES_CONTROLLER",
    );
    let too_large = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            after_cursor: "0".to_owned(),
            limit: 501,
            timeline_version: 1,
        })
        .await
        .unwrap_err();
    semantic_error(&too_large, "INVALID_ARGUMENT");
    let unsupported_version = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            after_cursor: "0".to_owned(),
            limit: 100,
            timeline_version: 2,
        })
        .await
        .unwrap_err();
    semantic_error(&unsupported_version, "UNSUPPORTED_SCHEMA_VERSION");
    let beyond_head = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            after_cursor: u64::MAX.to_string(),
            limit: 100,
            timeline_version: 1,
        })
        .await
        .unwrap_err();
    semantic_error(&beyond_head, "EVENT_CURSOR_INVALID");

    let first = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            after_cursor: "0".to_owned(),
            limit: 1,
            timeline_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first.items.len(), 1);
    let input = &first.items[0];
    assert_eq!(input.r#type, pb::TimelineItemType::UserInputAccepted as i32);
    assert_eq!(
        input.turn_id,
        accepted.accepted_turn.as_ref().unwrap().turn_id
    );
    assert_eq!(
        input.content,
        Some(pb::timeline_item::Content::InlineText(message.to_owned()))
    );
    assert_eq!(input.images.len(), 1);
    assert_eq!(input.images[0].ordinal, 0);
    assert_eq!(input.images[0].detail, pb::ImageDetail::High as i32);
    assert_eq!(input.images[0].media_type, "image/png");
    assert!(!format!("{input:?}").contains(image_path.to_str().unwrap()));
    let next = first.next_after_cursor.clone().unwrap();
    assert_eq!(next, input.cursor);

    let rest = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            after_cursor: next,
            limit: 100,
            timeline_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        rest.items
            .iter()
            .map(|item| item.r#type)
            .collect::<Vec<_>>(),
        [
            pb::TimelineItemType::AssistantResponseFinal as i32,
            pb::TimelineItemType::TurnTerminal as i32,
        ]
    );
    assert_eq!(
        rest.items[0].content,
        Some(pb::timeline_item::Content::InlineText(
            "타임라인 응답".to_owned()
        ))
    );
    assert!(rest.next_after_cursor.is_none());

    let replay = runs.submit_turn(request).await.unwrap().into_inner();
    assert_eq!(replay.accepted_turn, accepted.accepted_turn);
    let all = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            after_cursor: "0".to_owned(),
            limit: 100,
            timeline_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        all.items
            .iter()
            .filter(|item| item.r#type == pb::TimelineItemType::UserInputAccepted as i32)
            .count(),
        1
    );
    let prior_head = all.captured_head_cursor.clone();
    let current = native_snapshot(&mut runs, &fixture, &id).await;
    let long_message = "x".repeat(1_048_577);
    runs.submit_turn(pb::SubmitTurnRequest {
        context: context(),
        run: fixture.run_ref(&id),
        controller: fixture.carrier(),
        idempotency_key: "timeline-long-input".to_owned(),
        write_intent: pb::WriteIntent::Read as i32,
        message: long_message.clone(),
        images: Vec::new(),
        effort: None,
        expected_state_revision: current.state_revision,
    })
    .await
    .unwrap();
    native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Idle).await;
    let added = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            after_cursor: prior_head,
            limit: 100,
            timeline_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    let artifact = match added.items[0].content.as_ref().unwrap() {
        pb::timeline_item::Content::Artifact(artifact) => artifact,
        other => panic!("long accepted input was not an artifact: {other:?}"),
    };
    assert_eq!(artifact.kind, pb::ArtifactKind::UserInput as i32);
    assert_eq!(
        artifact.visibility,
        pb::ArtifactVisibility::ControllerOnly as i32
    );
    assert_eq!(artifact.byte_length, long_message.len() as u64);
    let mut artifacts = pb::artifact_service_client::ArtifactServiceClient::new(channel);
    let protected = artifacts
        .get_artifact(pb::GetArtifactRequest {
            context: context(),
            run: fixture.run_ref(&id),
            artifact_id: artifact.artifact_id.clone(),
            controller: None,
        })
        .await
        .unwrap_err();
    semantic_error(&protected, "INTERACTION_ARTIFACT_REQUIRES_CONTROLLER");
    let metadata = artifacts
        .get_artifact(pb::GetArtifactRequest {
            context: context(),
            run: fixture.run_ref(&id),
            artifact_id: artifact.artifact_id.clone(),
            controller: fixture.carrier(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(metadata.artifact.as_ref(), Some(artifact));
    let first_chunk = artifacts
        .read_artifact_chunk(pb::ReadArtifactChunkRequest {
            context: context(),
            run: fixture.run_ref(&id),
            artifact_id: artifact.artifact_id.clone(),
            offset: 0,
            length: 1_048_576,
            controller: fixture.carrier(),
        })
        .await
        .unwrap()
        .into_inner();
    let last_chunk = artifacts
        .read_artifact_chunk(pb::ReadArtifactChunkRequest {
            context: context(),
            run: fixture.run_ref(&id),
            artifact_id: artifact.artifact_id.clone(),
            offset: first_chunk.length as u64,
            length: 1,
            controller: fixture.carrier(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first_chunk.data.len(), 1_048_576);
    assert_eq!(last_chunk.data, b"x");
    assert!(last_chunk.eof);
    let cli = fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "timeline",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        &id,
    ]);
    assert_eq!(cli["items"][0]["content"]["text"], message);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_revision_action_barrier() {
    let fixture = Fixture::new("run_start_model_list.json");
    let mut gateway = fixture.start_gateway();
    let mut client = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let allocated = client
        .start_run(start_request(
            &fixture,
            "writer-barrier",
            pb::ExecutionLane::Dedicated,
        ))
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    let mut observation =
        pb::observation_service_client::ObservationServiceClient::new(gateway.channel().await);
    let mut events = observation
        .watch_run_events(pb::WatchRunEventsRequest {
            context: context(),
            run: fixture.run_ref(&allocated.run_id),
            after_cursor: "0".to_owned(),
            projection: pb::ProjectionProfile::Minimal as i32,
            projection_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    let stale_revision = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = events
                .message()
                .await
                .unwrap()
                .expect("fresh Run emits a durable lifecycle event");
            if let Some(pb::run_event_envelope::Item::DurableEvent(event)) = envelope.item {
                let stamp = event.stamp.expect("event carries its historical revision");
                assert_eq!(stamp.captured_head_cursor, event.cursor);
                break stamp.run_state_revision;
            }
        }
    })
    .await
    .expect("fresh Run event replay is available");
    drop(events);
    assert!(stale_revision <= allocated.state_revision);
    let paused = client
        .pause_run(pb::PauseRunRequest {
            context: context(),
            run: fixture.run_ref(&allocated.run_id),
            controller: fixture.carrier(),
            interrupt: false,
            expected_state_revision: allocated.state_revision,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert!(paused.state_revision > stale_revision);
    assert_eq!(paused.lifecycle, pb::RunLifecycle::Paused as i32);
    let before = audit(&fixture, &allocated.run_id);
    let upstream_before = upstream_turn_starts(&fixture);
    let writer_path = fixture.state_root.join("runtime/writer.json");
    let writer_before = fs::read(&writer_path).unwrap();
    let rejected = client
        .submit_turn(pb::SubmitTurnRequest {
            context: context(),
            run: fixture.run_ref(&allocated.run_id),
            controller: fixture.carrier(),
            idempotency_key: "stale-write".to_owned(),
            write_intent: pb::WriteIntent::Write as i32,
            message: "This stale request must not reserve writer authority.".to_owned(),
            images: Vec::new(),
            effort: None,
            expected_state_revision: stale_revision,
        })
        .await
        .unwrap_err();
    assert_eq!(rejected.code(), Code::Aborted);
    semantic_error(&rejected, "RUN_STATE_CONFLICT");
    assert_eq!(audit(&fixture, &allocated.run_id), before);
    assert_eq!(fs::read(&writer_path).unwrap(), writer_before);
    let rejected = client
        .resume_run(pb::ResumeRunRequest {
            context: context(),
            run: fixture.run_ref(&allocated.run_id),
            controller: fixture.carrier(),
            expected_state_revision: stale_revision,
        })
        .await
        .unwrap_err();
    semantic_error(&rejected, "RUN_STATE_CONFLICT");
    assert_eq!(audit(&fixture, &allocated.run_id), before);
    let zero_revision = client
        .resume_run(pb::ResumeRunRequest {
            context: context(),
            run: fixture.run_ref(&allocated.run_id),
            controller: fixture.carrier(),
            expected_state_revision: 0,
        })
        .await
        .unwrap_err();
    semantic_error(&zero_revision, "RUN_STATE_CONFLICT");
    assert_eq!(audit(&fixture, &allocated.run_id), before);
    let runtime_path = dolgorae::worker::runtime_record_path(
        &dolgorae::worker::runtime_root(&fixture.state_root),
        Uuid::parse_str(&allocated.run_id).unwrap(),
    )
    .unwrap();
    assert!(
        !runtime_path.exists(),
        "stale mutations must not start a worker"
    );

    let read = client
        .start_run(start_request(
            &fixture,
            "accepted-replay",
            pb::ExecutionLane::SharedReadonly,
        ))
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    let request = pb::SubmitTurnRequest {
        context: context(),
        run: fixture.run_ref(&read.run_id),
        controller: fixture.carrier(),
        idempotency_key: "original-turn".to_owned(),
        write_intent: pb::WriteIntent::Read as i32,
        message: "Produce one fixture turn.".to_owned(),
        images: Vec::new(),
        effort: None,
        expected_state_revision: read.state_revision,
    };
    let accepted = client
        .submit_turn(request.clone())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        accepted.accepted_turn.as_ref().unwrap().status,
        pb::TurnStatus::Accepted as i32
    );
    let terminal =
        native_wait_lifecycle(&mut client, &fixture, &read.run_id, pb::RunLifecycle::Idle).await;
    assert!(terminal.state_revision > accepted.run.as_ref().unwrap().state_revision);
    let replayed = client
        .submit_turn(request.clone())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(replayed.accepted_turn, accepted.accepted_turn);
    assert_eq!(replayed.run, accepted.run);
    assert_eq!(
        replayed.writer.as_ref().unwrap().stamp,
        accepted.writer.as_ref().unwrap().stamp
    );
    assert_eq!(replayed.correlation_id, accepted.correlation_id);
    assert_eq!(turn_starts(&fixture, &read.run_id), 1);
    assert_eq!(upstream_turn_starts(&fixture), upstream_before + 1);
    let before = audit(&fixture, &read.run_id);
    let mut changed = request;
    changed.message.push_str(" Changed input.");
    let rejected = client.submit_turn(changed).await.unwrap_err();
    semantic_error(&rejected, "IDEMPOTENCY_CONFLICT");
    assert_eq!(audit(&fixture, &read.run_id), before);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_run_allocation_replay() {
    let fixture = Fixture::new("run_start_model_list.json");
    let mut gateway = fixture.start_gateway();
    let mut client = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let request = start_request(&fixture, "response-loss", pb::ExecutionLane::SharedReadonly);
    let first = client
        .start_run(request.clone())
        .await
        .unwrap()
        .into_inner();
    assert!(!first.exact_replay);
    let run_id = first.run.as_ref().unwrap().run_id.clone();
    assert_eq!(run_count(&fixture), 1);
    gateway.kill();
    let mut gateway = fixture.start_gateway();
    let mut client = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let replay = client
        .start_run(request.clone())
        .await
        .unwrap()
        .into_inner();
    assert!(replay.exact_replay);
    assert_eq!(replay.run.as_ref().unwrap().run_id, run_id);
    assert_eq!(run_count(&fixture), 1);

    // Reproduce the durable boundary before publication using the reservation
    // store itself. Normalized allocation input excludes its idempotency key.
    let reservations = StartReservationStore::new(SystemWorkspacePlatform, &fixture.state_root);
    let original = reservations.load("response-loss").unwrap().unwrap();
    let reserved_id = Uuid::now_v7();
    reservations
        .reserve(&StartReservation {
            schema_version: 1,
            operation: "start_run".to_owned(),
            idempotency_key: "before-publication".to_owned(),
            normalized_identity_sha256: original.normalized_identity_sha256,
            run_id: reserved_id,
        })
        .unwrap();
    let mut prepared = request.clone();
    prepared.idempotency_key = "before-publication".to_owned();
    let mut drift = prepared.clone();
    drift.instructions = Some("Different normalized allocation input.".to_owned());
    let rejected = client.start_run(drift).await.unwrap_err();
    semantic_error(&rejected, "IDEMPOTENCY_CONFLICT");
    assert_eq!(run_count(&fixture), 1);
    assert!(
        !fixture
            .state_root
            .join("runs")
            .join(reserved_id.to_string())
            .exists()
    );
    let recovered = client
        .start_run(prepared.clone())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        recovered.run.as_ref().unwrap().run_id,
        reserved_id.to_string()
    );
    assert_eq!(run_count(&fixture), 2);
    let retry = client.start_run(prepared).await.unwrap().into_inner();
    assert!(retry.exact_replay);
    assert_eq!(retry.run.as_ref().unwrap().run_id, reserved_id.to_string());
    assert_eq!(run_count(&fixture), 2);

    let rejected = client
        .delete_run(pb::DeleteRunRequest {
            context: context(),
            run: fixture.run_ref(&run_id),
            controller: fixture.carrier(),
            confirm: true,
            expected_state_revision: replay.run.unwrap().state_revision,
        })
        .await
        .unwrap_err();
    semantic_error(&rejected, "CAPABILITY_UNSUPPORTED");
    assert_eq!(run_count(&fixture), 2);
    assert!(
        client
            .start_run(request)
            .await
            .unwrap()
            .into_inner()
            .exact_replay
    );
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orchestrated_start_run_pins_policy_and_replays_without_registry_source() {
    let fixture = Fixture::new("run_start_model_list.json");
    let role_directory = fixture.workspace.join(".dolgorae/roles");
    fs::create_dir_all(&role_directory).unwrap();
    let role_path = role_directory.join("reviewer.json");
    fs::write(
        &role_path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version":1,
            "name":"reviewer",
            "display_name":"Reviewer",
            "description":"Reviews bounded implementation evidence.",
            "instructions":"Inspect the evidence and return one bounded verdict.",
        }))
        .unwrap(),
    )
    .unwrap();
    let policy_input = fixture.root.join("specialist-policy-input.json");
    fs::write(
        &policy_input,
        serde_json::to_vec(&serde_json::json!({
            "schema_version":2,
            "policy_name":"brokered-review",
            "revision":1,
            "approval_policy":"fully_delegated",
            "max_active_specialists":1,
            "roles":[{
                "role_ref":"reviewer",
                "role_source":{"scope":"project","name":"reviewer"},
                "agent_configuration":{
                    "schema_version":2,
                    "selected_profile":"default",
                    "model":"gpt-5.6",
                    "default_effort":"medium",
                    "purpose":"review",
                    "purpose_label":null,
                    "required_capabilities":[],
                    "execution_lane":"dedicated",
                    "required_assurance":"best_effort_personal_alpha",
                    "native_subagent_policy":"enabled"
                },
                "max_active_instances":1,
                "reuse_policy":"never",
                "allowed_access":["read_only"],
                "activation_policy":"keep_resident",
                "primary_may_request":true,
                "collaboration_source":false,
                "collaboration_target":false,
                "auto_approve_when_fully_delegated":true
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let stopped = fixture.cli(&[
        "profile",
        "server",
        "stop",
        "default",
        "--operator-file",
        fixture.operator.to_str().unwrap(),
    ]);
    assert_eq!(stopped["stopped"], true);
    let rejected = fixture.command(&[
        "specialist",
        "policy",
        "validate",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--file",
        policy_input.to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    let rejection: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(rejection["error"]["code"], "COMPATIBILITY_REJECTED");
    let status = fixture.cli(&["profile", "server", "status", "default"]);
    assert_eq!(status["lifecycle"], "stopped");
    fixture.cli(&["profile", "server", "start", "default"]);
    let validated = fixture.cli(&[
        "specialist",
        "policy",
        "validate",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--file",
        policy_input.to_str().unwrap(),
    ]);
    let installed = fixture.cli(&[
        "specialist",
        "policy",
        "add",
        "brokered-review",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--file",
        policy_input.to_str().unwrap(),
    ]);
    assert_eq!(installed["schema_version"], 2);
    assert_eq!(installed, validated);
    assert_eq!(installed["roles"][0]["role"]["name"], "reviewer");
    let listed = fixture.cli(&[
        "specialist",
        "policy",
        "list",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert_eq!(listed["items"], serde_json::json!([installed]));
    let carrier = fixture
        .controller
        .parent()
        .unwrap()
        .join("orchestrated.json");
    let created = fixture.cli(&[
        "controller",
        "credential",
        "create",
        "--kind",
        "interactive-client",
        "--instance-id",
        "orchestrated-native-test",
        "--orchestration-policy",
        "brokered-review",
        "--output",
        carrier.to_str().unwrap(),
    ]);
    let controller_id = created["controller"]["controller_id"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.cli(&[
        "profile",
        "server",
        "stop",
        "default",
        "--operator-file",
        fixture.operator.to_str().unwrap(),
    ]);
    assert_eq!(
        fixture.cli(&["profile", "remove", "default"])["removed"],
        true
    );
    let missing = fixture.command(&[
        "run",
        "--controller-file",
        carrier.to_str().unwrap(),
        "start",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--profile",
        "default",
        "--control-mode",
        "direct-interactive",
        "--execution-lane",
        "shared-readonly",
        "--required-assurance",
        "best-effort-personal-alpha",
        "--purpose",
        "interactive",
        "--instructions",
        "Exercise missing policy binding rejection.",
        "--idempotency-key",
        "orchestrated-missing-binding",
    ]);
    assert!(!missing.status.success());
    let missing: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(missing["error"]["code"], "PROFILE_NOT_FOUND");
    let codex = fixture.root.join("bin/codex");
    let codex_home = fixture.root.join("codex-home");
    fixture.cli(&[
        "profile",
        "add",
        "default",
        "--codex-home",
        codex_home.to_str().unwrap(),
        "--native-subagents",
        "enabled",
        "--env",
        "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
        "--env",
        "LANG=en_US.UTF-8",
        "--env",
        "LC_ALL=en_US.UTF-8",
        "--env",
        "EXAMPLE_POLICY_DRIFT=1",
        "--",
        codex.to_str().unwrap(),
    ]);
    let drifted = fixture.command(&[
        "run",
        "--controller-file",
        carrier.to_str().unwrap(),
        "start",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--profile",
        "default",
        "--control-mode",
        "direct-interactive",
        "--execution-lane",
        "shared-readonly",
        "--required-assurance",
        "best-effort-personal-alpha",
        "--purpose",
        "interactive",
        "--instructions",
        "Exercise changed policy binding rejection.",
        "--idempotency-key",
        "orchestrated-changed-binding",
    ]);
    assert!(!drifted.status.success());
    let drifted: Value = serde_json::from_slice(&drifted.stdout).unwrap();
    assert_eq!(drifted["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(run_count(&fixture), 0);
    assert_eq!(
        fixture.cli(&["profile", "server", "status", "default"])["lifecycle"],
        "ready"
    );
    fixture.cli(&[
        "profile",
        "server",
        "stop",
        "default",
        "--operator-file",
        fixture.operator.to_str().unwrap(),
    ]);
    assert_eq!(
        fixture.cli(&["profile", "remove", "default"])["removed"],
        true
    );
    fixture.cli(&[
        "profile",
        "add",
        "default",
        "--codex-home",
        codex_home.to_str().unwrap(),
        "--native-subagents",
        "enabled",
        "--env",
        "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
        "--env",
        "LANG=en_US.UTF-8",
        "--env",
        "LC_ALL=en_US.UTF-8",
        "--",
        codex.to_str().unwrap(),
    ]);
    fixture.cli(&["profile", "server", "start", "default"]);
    let mut out_of_slice: Value =
        serde_json::from_slice(&fs::read(&policy_input).unwrap()).unwrap();
    out_of_slice["policy_name"] = Value::String("out-of-slice-review".to_owned());
    out_of_slice["roles"][0]["reuse_policy"] = Value::String("reuse_any_compatible".to_owned());
    let out_of_slice_path = fixture.root.join("out-of-slice-policy.json");
    fs::write(
        &out_of_slice_path,
        serde_json::to_vec(&out_of_slice).unwrap(),
    )
    .unwrap();
    fixture.cli(&[
        "specialist",
        "policy",
        "add",
        "out-of-slice-review",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--file",
        out_of_slice_path.to_str().unwrap(),
    ]);
    let out_of_slice_carrier = fixture.root.join("out-of-slice-controller.json");
    fixture.cli(&[
        "controller",
        "credential",
        "create",
        "--kind",
        "interactive-client",
        "--instance-id",
        "out-of-slice-test",
        "--orchestration-policy",
        "out-of-slice-review",
        "--output",
        out_of_slice_carrier.to_str().unwrap(),
    ]);
    let refused = fixture.command(&[
        "run",
        "--controller-file",
        out_of_slice_carrier.to_str().unwrap(),
        "start",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--profile",
        "default",
        "--control-mode",
        "direct-interactive",
        "--execution-lane",
        "shared-readonly",
        "--required-assurance",
        "best-effort-personal-alpha",
        "--purpose",
        "interactive",
        "--instructions",
        "Reject an unsupported live policy before allocation.",
        "--idempotency-key",
        "orchestrated-out-of-slice",
    ]);
    assert!(!refused.status.success());
    let refused: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(refused["error"]["code"], "POLICY_REJECTED");
    assert_eq!(run_count(&fixture), 0);
    fs::remove_file(role_path).unwrap();
    let carrier_ref = pb::ControllerCarrierRef {
        absolute_file_path: carrier.to_string_lossy().into_owned(),
        expected_controller_id: controller_id,
        expected_controller_generation: 1,
    };
    let mut request = start_request(
        &fixture,
        "orchestrated-response-loss",
        pb::ExecutionLane::SharedReadonly,
    );
    request.controller = Some(carrier_ref);

    let mut gateway = fixture.start_gateway();
    let mut client = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    semantic_error(
        &client.start_run(request.clone()).await.unwrap_err(),
        "INVALID_ARGUMENT",
    );
    let mut parented = request.clone();
    parented.idempotency_key = "orchestrated-parented-denial".to_owned();
    parented.control_mode = pb::ControlMode::DirectInteractive as i32;
    parented.parent = Some(pb::ParentRefProjection {
        namespace: "example.client.v1".to_owned(),
        kind: "parent".to_owned(),
        id: "forged".to_owned(),
    });
    semantic_error(
        &client.start_run(parented).await.unwrap_err(),
        "INVALID_ARGUMENT",
    );
    assert_eq!(run_count(&fixture), 0);
    request.control_mode = pb::ControlMode::DirectInteractive as i32;
    let first = client
        .start_run(request.clone())
        .await
        .unwrap()
        .into_inner();
    assert!(!first.exact_replay);
    let run_id = Uuid::parse_str(&first.run.as_ref().unwrap().run_id).unwrap();
    let session = dolgorae::orchestration::OrchestrationStore::open(&fixture.state_root)
        .unwrap()
        .session(run_id)
        .unwrap();
    assert_eq!(session.status, "active");
    assert_eq!(session.root_run_id, run_id);
    assert_eq!(
        serde_json::to_value(&session.specialist_policy).unwrap(),
        installed
    );
    let manifest: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .state_root
                .join("runs")
                .join(run_id.to_string())
                .join("manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["aggregate_binding"]["aggregate_kind"],
        "orchestrated_session"
    );
    assert_eq!(manifest["aggregate_binding"]["member_kind"], "primary");
    assert_eq!(
        manifest["aggregate_binding"]["policy_sha256"],
        session.specialist_policy_sha256
    );

    let removed = fixture.cli(&[
        "specialist",
        "policy",
        "remove",
        "brokered-review",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert_eq!(removed, serde_json::json!({"deleted":true}));
    gateway.kill();
    let mut gateway = fixture.start_gateway();
    let mut client = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let replay = client
        .start_run(request.clone())
        .await
        .unwrap()
        .into_inner();
    assert!(replay.exact_replay);
    assert_eq!(replay.run.as_ref().unwrap().run_id, run_id.to_string());
    assert_eq!(run_count(&fixture), 1);
    let replayed_run = replay.run.unwrap();
    let controller = request.controller.clone();
    let mut drift = request;
    drift.instructions = Some("Different orchestration request.".to_owned());
    semantic_error(
        &client.start_run(drift).await.unwrap_err(),
        "IDEMPOTENCY_CONFLICT",
    );
    assert_eq!(run_count(&fixture), 1);
    let closed = client
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: fixture.run_ref(&run_id.to_string()),
            controller,
            interrupt: false,
            expected_state_revision: replayed_run.state_revision,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert_eq!(closed.lifecycle, pb::RunLifecycle::Closed as i32);
    gateway.terminate();
}

fn active_turn_scenario(interaction: bool) -> Value {
    let mut scenario = support::base_scenario();
    let steps = scenario["steps"].as_array_mut().unwrap();
    steps.retain(|step| step["method"] != "turn/start" && step["method"] != "thread/read");
    steps.push(serde_json::json!({
        "method":"turn/start", "respond":{"result":{"turn":{"id":"turn-native-active"}}},
        "emit": if interaction { serde_json::json!([{
            "kind":"request", "id":7301, "method":"item/tool/requestUserInput",
            "params":{"threadId":"${thread_id}","turnId":"turn-native-active",
                "questions":[{"id":"choice","header":"Choice","question":"Choose the next step",
                    "isSecret":false,"isOther":true,"options":[]}]}
        }]) } else { serde_json::json!([]) }
    }));
    steps.push(serde_json::json!({
        "method":"turn/interrupt", "respond":{"result":{"interrupted":true}},
        "emit":[{"kind":"notification","method":"turn/completed","params":{
            "threadId":"${thread_id}","turn":{"id":"turn-native-active","status":"interrupted","items":[]}
        }}]
    }));
    // The bootstrap probe asks for a nonexistent random Thread. A later
    // recovery reads the completed history of the accepted test Turn.
    steps.push(serde_json::json!({"method":"thread/read","occurrence":1,
        "respond":{"error":{"code":-32600,"message":"thread not found"}}}));
    steps.push(serde_json::json!({"method":"thread/read","respond":{"result":{"thread":{
        "id":"thread-alpha","turns":[{"id":"turn-native-active","status":"completed","items":[]}]
    }}}}));
    scenario
}

fn timeline_scenario() -> Value {
    let mut scenario = support::base_scenario();
    let step = scenario["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|step| step["method"] == "turn/start" && step["occurrence"] == 1)
        .unwrap();
    step["emit"] = serde_json::json!([{"kind":"notification","method":"turn/completed","params":{
        "threadId":"${thread_id}","turn":{"id":"turn-1","status":"completed","items":[{
            "type":"agentMessage","status":"completed","phase":"final_answer","text":"타임라인 응답"
        }]}
    }}]);
    scenario
}

fn run_frozen_go_consumer(
    fixture: &Fixture,
    gateway: &support::Gateway,
    run_id: &str,
    controller: &pb::ControllerCarrierRef,
    module: &str,
    test_name: &str,
    phase: &str,
) {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let go_cache = repository.join("target/frozen-go-cache");
    fs::create_dir_all(go_cache.join("mod")).unwrap();
    fs::create_dir_all(go_cache.join("build")).unwrap();
    fs::create_dir_all(go_cache.join("path")).unwrap();
    let output = Command::new("go")
        .args([
            "test",
            "./dolgorae/public/v1",
            "-run",
            test_name,
            "-count=1",
            "-v",
        ])
        .current_dir(repository.join(module))
        .env("GOTOOLCHAIN", "local")
        .env("GOMODCACHE", go_cache.join("mod"))
        .env("GOCACHE", go_cache.join("build"))
        .env("GOPATH", go_cache.join("path"))
        .env("GOENV", "off")
        .env("GOTELEMETRY", "off")
        .env("DOLGORAE_REPOSITORY", repository)
        .env("DOLGORAE_FROZEN_SOCKET", &gateway.socket)
        .env("DOLGORAE_FROZEN_WORKSPACE", &fixture.workspace)
        .env("DOLGORAE_FROZEN_WORKSPACE_ID", &fixture.workspace_id)
        .env("DOLGORAE_FROZEN_RUN_ID", run_id)
        .env("DOLGORAE_FROZEN_CONTROLLER", &controller.absolute_file_path)
        .env(
            "DOLGORAE_FROZEN_CONTROLLER_ID",
            &controller.expected_controller_id,
        )
        .env(
            "DOLGORAE_FROZEN_DESCRIPTOR_SHA256",
            dolgorae::protocol::PUBLIC_V1_DESCRIPTOR_SHA256,
        )
        .env("DOLGORAE_FROZEN_PHASE", phase)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "frozen consumer failed ({module}, {phase}):\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_generated_consumers_run_unchanged_against_candidate_and_restart() {
    let fixture = Fixture::with_scenario(timeline_scenario());
    let controller = install_orchestration_controller(&fixture, "frozen-consumer");
    let mut gateway = fixture.start_gateway();
    let mut runs = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let started = runs
        .start_run(pb::StartRunRequest {
            context: context(),
            workspace: Some(fixture.workspace()),
            controller: Some(controller.clone()),
            idempotency_key: Uuid::now_v7().to_string(),
            profile_name: fixture.profile.clone(),
            control_mode: pb::ControlMode::DirectInteractive as i32,
            execution_lane: pb::ExecutionLane::SharedReadonly as i32,
            purpose: pb::PurposeKind::Interactive as i32,
            purpose_label: None,
            model: Some("gpt-5.6".to_owned()),
            effort: Some("medium".to_owned()),
            required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
            required_capabilities: Vec::new(),
            instructions: Some("Exercise the frozen generated clients.".to_owned()),
            parent: None,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();

    run_frozen_go_consumer(
        &fixture,
        &gateway,
        &started.run_id,
        &controller,
        "docs/protocol/generated/gul-consumer-v1/go",
        "TestFrozenConsumerAgainstCandidate",
        "active",
    );
    run_frozen_go_consumer(
        &fixture,
        &gateway,
        &started.run_id,
        &controller,
        "docs/protocol/generated/pre-task-053-low-level/go",
        "TestPreExtensionClientAgainstCandidate",
        "active",
    );
    run_frozen_go_consumer(
        &fixture,
        &gateway,
        &started.run_id,
        &controller,
        "docs/protocol/generated/gul-consumer-v1/go",
        "TestFrozenConsumerAgainstCandidate",
        "close",
    );

    gateway.terminate();
    let mut gateway = fixture.start_gateway();
    run_frozen_go_consumer(
        &fixture,
        &gateway,
        &started.run_id,
        &controller,
        "docs/protocol/generated/gul-consumer-v1/go",
        "TestFrozenConsumerAgainstCandidate",
        "recovered",
    );
    run_frozen_go_consumer(
        &fixture,
        &gateway,
        &started.run_id,
        &controller,
        "docs/protocol/generated/pre-task-053-low-level/go",
        "TestPreExtensionClientAgainstCandidate",
        "recovered",
    );
    gateway.terminate();
}

async fn native_snapshot(
    runs: &mut pb::run_service_client::RunServiceClient<tonic::transport::Channel>,
    fixture: &Fixture,
    id: &str,
) -> pb::RunProjection {
    runs.get_run(pb::GetRunRequest {
        context: context(),
        run: fixture.run_ref(id),
    })
    .await
    .unwrap()
    .into_inner()
    .run
    .unwrap()
}

async fn native_wait_lifecycle(
    runs: &mut pb::run_service_client::RunServiceClient<tonic::transport::Channel>,
    fixture: &Fixture,
    id: &str,
    lifecycle: pb::RunLifecycle,
) -> pb::RunProjection {
    let state_path = fixture.state_root.join("runs").join(id).join("state.json");
    let expected = match lifecycle {
        pb::RunLifecycle::Idle => "idle",
        pb::RunLifecycle::Running => "running",
        pb::RunLifecycle::WaitingInteraction => "waiting_interaction",
        pb::RunLifecycle::Paused => "paused",
        pb::RunLifecycle::Closed => "closed",
        pb::RunLifecycle::OutcomeUnknown => "outcome_unknown",
        _ => panic!("unsupported test lifecycle"),
    };
    // Wait on the durable publication barrier, then verify it through one
    // public snapshot. Repeated native GetRun calls would each perform the
    // required five-sample OS workload census for a dedicated Run.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
            if state["lifecycle"] == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "Run {id} did not reach {lifecycle:?}: {}",
            fs::read_to_string(&state_path).unwrap()
        )
    });
    let snapshot = native_snapshot(runs, fixture, id).await;
    assert_eq!(
        snapshot.lifecycle, lifecycle as i32,
        "public snapshot disagrees with durable lifecycle"
    );
    snapshot
}

async fn native_submit(
    runs: &mut pb::run_service_client::RunServiceClient<tonic::transport::Channel>,
    fixture: &Fixture,
    snapshot: &pb::RunProjection,
    intent: pb::WriteIntent,
) -> pb::SubmitTurnAccepted {
    runs.submit_turn(pb::SubmitTurnRequest {
        context: context(),
        run: fixture.run_ref(&snapshot.run_id),
        controller: fixture.carrier(),
        idempotency_key: Uuid::now_v7().to_string(),
        write_intent: intent as i32,
        message: "Exercise native lifecycle transition.".into(),
        images: vec![],
        effort: None,
        expected_state_revision: snapshot.state_revision,
    })
    .await
    .unwrap()
    .into_inner()
}

fn transcript_methods(fixture: &Fixture, method: &str) -> Vec<Value> {
    fs::read_to_string(fixture.root.join("transcript.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|message| message["method"] == method)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_interrupt_invalidates_interaction() {
    let fixture = Fixture::with_scenario(active_turn_scenario(true));
    let id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut interactions =
        pb::interaction_service_client::InteractionServiceClient::new(channel.clone());
    let mut observations = pb::observation_service_client::ObservationServiceClient::new(channel);
    let initial = native_snapshot(&mut runs, &fixture, &id).await;
    native_submit(&mut runs, &fixture, &initial, pb::WriteIntent::Read).await;
    let waiting = native_wait_lifecycle(
        &mut runs,
        &fixture,
        &id,
        pb::RunLifecycle::WaitingInteraction,
    )
    .await;
    let orchestration_db =
        dolgorae::engagement::EngagementStore::workspace_database_path(&fixture.state_root);
    assert!(!orchestration_db.exists());
    let pending = interactions
        .list_pending_interactions(pb::ListPendingInteractionsRequest {
            context: context(),
            run: fixture.run_ref(&id),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(pending.items.len(), 1);
    assert!(!orchestration_db.exists());
    let interaction_id = pending.items[0].interaction_id.clone();
    let before = audit(&fixture, &id);
    let interrupts = transcript_methods(&fixture, "turn/interrupt").len();
    let stale = runs
        .interrupt_turn(pb::InterruptTurnRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            expected_state_revision: 0,
        })
        .await
        .unwrap_err();
    semantic_error(&stale, "RUN_STATE_CONFLICT");
    assert_eq!(audit(&fixture, &id), before);
    let stale = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            interrupt: true,
            expected_state_revision: 0,
        })
        .await
        .unwrap_err();
    semantic_error(&stale, "RUN_STATE_CONFLICT");
    assert_eq!(audit(&fixture, &id), before);
    assert_eq!(
        transcript_methods(&fixture, "turn/interrupt").len(),
        interrupts
    );
    let interrupted = runs
        .interrupt_turn(pb::InterruptTurnRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            expected_state_revision: waiting.state_revision,
        })
        .await;
    // The acknowledgement can race the terminal publications. A bounded
    // response capture may ask for a fresh snapshot after the interrupt took
    // effect; never repeat the mutation to obtain that snapshot.
    if let Err(status) = interrupted {
        assert_eq!(status.code(), Code::Aborted);
        let detail = semantic_error(&status, "RUN_STATE_CONFLICT");
        assert_eq!(
            detail.action,
            pb::RequiredClientAction::RefreshSnapshot as i32
        );
    }
    let terminal = native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Idle).await;
    assert!(terminal.active_turn.is_none());
    assert_eq!(terminal.pending_interaction_count, 0);
    let terminal_statuses: Vec<_> = audit(&fixture, &id)
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .filter(|record| record["kind"] == "turn_terminal")
        .map(|record| record["payload"]["status"].clone())
        .collect();
    assert_eq!(terminal_statuses, [serde_json::json!("interrupted")]);
    let sent = transcript_methods(&fixture, "turn/interrupt");
    assert_eq!(sent.len(), interrupts + 1);
    assert_eq!(
        sent.last().unwrap()["params"]["turnId"],
        "turn-native-active"
    );
    let interaction = interactions
        .get_controller_interaction(pb::GetControllerInteractionRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            interaction_id,
        })
        .await
        .unwrap()
        .into_inner()
        .interaction
        .unwrap();
    assert_eq!(
        interaction.summary.unwrap().status,
        pb::InteractionStatus::Stale as i32
    );
    let records = audit(&fixture, &id);
    assert!(
        records
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .any(|record| record["kind"] == "turn_terminal"
                && record["payload"]["status"] == "interrupted")
    );
    let timeline = observations
        .list_run_timeline_items(pb::ListRunTimelineItemsRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            after_cursor: "0".to_owned(),
            limit: 100,
            timeline_version: 1,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        timeline
            .items
            .iter()
            .map(|item| item.r#type)
            .collect::<Vec<_>>(),
        [
            pb::TimelineItemType::UserInputAccepted as i32,
            pb::TimelineItemType::InteractionOpened as i32,
            pb::TimelineItemType::InteractionResolved as i32,
            pb::TimelineItemType::TurnTerminal as i32,
        ]
    );
    assert_eq!(
        timeline.items[1].interaction_status,
        Some(pb::InteractionStatus::Pending as i32)
    );
    assert_eq!(
        timeline.items[2].interaction_status,
        Some(pb::InteractionStatus::Stale as i32)
    );
    assert_eq!(
        timeline.items[3].status,
        Some(pb::TimelineItemStatus::Interrupted as i32)
    );
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_pending_interaction_parity_after_lost_interrupt_response() {
    let mut scenario = active_turn_scenario(true);
    let interrupt = scenario["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|step| step["method"] == "turn/interrupt")
        .unwrap();
    interrupt["respond"] = serde_json::json!({"silent":true});
    interrupt["emit"] = serde_json::json!([{"kind":"close","code":1011}]);

    let fixture = Fixture::with_scenario(scenario);
    let id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut interactions = pb::interaction_service_client::InteractionServiceClient::new(channel);
    let initial = native_snapshot(&mut runs, &fixture, &id).await;
    native_submit(&mut runs, &fixture, &initial, pb::WriteIntent::Read).await;
    let waiting = native_wait_lifecycle(
        &mut runs,
        &fixture,
        &id,
        pb::RunLifecycle::WaitingInteraction,
    )
    .await;

    let grpc_pending = interactions
        .list_pending_interactions(pb::ListPendingInteractionsRequest {
            context: context(),
            run: fixture.run_ref(&id),
        })
        .await
        .unwrap()
        .into_inner();
    let cli_pending = fixture.cli(&[
        "run",
        "pending",
        &id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert_eq!(grpc_pending.items.len(), 1);
    assert_eq!(cli_pending["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        cli_pending["items"][0]["request_id"],
        grpc_pending.items[0].interaction_id
    );

    let _ = runs
        .interrupt_turn(pb::InterruptTurnRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            expected_state_revision: waiting.state_revision,
        })
        .await;
    let unknown =
        native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::OutcomeUnknown).await;

    let durable_records: Vec<Value> = audit(&fixture, &id)
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    let requested: Vec<&Value> = durable_records
        .iter()
        .filter(|record| record["kind"] == "approval_requested")
        .collect();
    assert_eq!(requested.len(), 1);
    assert_eq!(requested[0]["payload"]["interaction"]["status"], "pending");
    assert!(
        !durable_records
            .iter()
            .any(|record| record["kind"] == "approval_decided")
    );

    let grpc_pending = interactions
        .list_pending_interactions(pb::ListPendingInteractionsRequest {
            context: context(),
            run: fixture.run_ref(&id),
        })
        .await
        .unwrap()
        .into_inner();
    let cli_pending = fixture.cli(&[
        "run",
        "pending",
        &id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert!(grpc_pending.items.is_empty());
    assert!(cli_pending["items"].as_array().unwrap().is_empty());
    let runtime_path = dolgorae::worker::runtime_record_path(
        &dolgorae::worker::runtime_root(&fixture.state_root),
        Uuid::parse_str(&id).unwrap(),
    )
    .unwrap();
    let runtime: dolgorae::worker::WorkerRuntimeRecord =
        serde_json::from_slice(&fs::read(&runtime_path).unwrap()).unwrap();
    let exit = dolgorae::darwin::DarwinSystem
        .watch_process_exit(runtime.identity.pid)
        .unwrap();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-KILL", &runtime.identity.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !exit.exited().unwrap() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("transport-lost worker remained alive during cleanup");
    let recovered = runs
        .recover_run(pb::RecoverRunRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            expected_state_revision: unknown.state_revision,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert_eq!(recovered.lifecycle, pb::RunLifecycle::Paused as i32);
    let grpc_pending = interactions
        .list_pending_interactions(pb::ListPendingInteractionsRequest {
            context: context(),
            run: fixture.run_ref(&id),
        })
        .await
        .unwrap()
        .into_inner();
    let cli_pending = fixture.cli(&[
        "run",
        "pending",
        &id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
    ]);
    assert!(grpc_pending.items.is_empty());
    assert!(cli_pending["items"].as_array().unwrap().is_empty());
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_close_and_pause_interrupt() {
    for close in [false, true] {
        let fixture = Fixture::with_scenario(active_turn_scenario(false));
        let id = fixture.start_run(&[]);
        let mut gateway = fixture.start_gateway();
        let mut runs = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
        let initial = native_snapshot(&mut runs, &fixture, &id).await;
        native_submit(&mut runs, &fixture, &initial, pb::WriteIntent::Read).await;
        let active =
            native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Running).await;
        let after = if close {
            runs.close_run(pb::CloseRunRequest {
                context: context(),
                run: fixture.run_ref(&id),
                controller: fixture.carrier(),
                interrupt: true,
                expected_state_revision: active.state_revision,
            })
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap()
        } else {
            runs.pause_run(pb::PauseRunRequest {
                context: context(),
                run: fixture.run_ref(&id),
                controller: fixture.carrier(),
                interrupt: true,
                expected_state_revision: active.state_revision,
            })
            .await
            .unwrap()
            .into_inner()
            .run
            .unwrap()
        };
        assert_eq!(
            after.lifecycle,
            if close {
                pb::RunLifecycle::Closed
            } else {
                pb::RunLifecycle::Paused
            } as i32
        );
        assert!(after.active_turn.is_none());
        assert_eq!(transcript_methods(&fixture, "turn/interrupt").len(), 1);
        if !close {
            let before = audit(&fixture, &id);
            let stale = runs
                .close_run(pb::CloseRunRequest {
                    context: context(),
                    run: fixture.run_ref(&id),
                    controller: fixture.carrier(),
                    interrupt: false,
                    expected_state_revision: initial.state_revision,
                })
                .await
                .unwrap_err();
            semantic_error(&stale, "RUN_STATE_CONFLICT");
            assert_eq!(
                audit(&fixture, &id),
                before,
                "stale offline close changed the audit"
            );
            let closed = runs
                .close_run(pb::CloseRunRequest {
                    context: context(),
                    run: fixture.run_ref(&id),
                    controller: fixture.carrier(),
                    interrupt: false,
                    expected_state_revision: after.state_revision,
                })
                .await
                .unwrap()
                .into_inner()
                .run
                .unwrap();
            assert_eq!(closed.lifecycle, pb::RunLifecycle::Closed as i32);
            assert_eq!(transcript_methods(&fixture, "turn/interrupt").len(), 1);
        }
        gateway.terminate();
    }
    // Closing an idle, previously active Run seals it without another Turn.
    let fixture = Fixture::new("run_start_model_list.json");
    let id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let mut runs = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
    let initial = native_snapshot(&mut runs, &fixture, &id).await;
    native_submit(&mut runs, &fixture, &initial, pb::WriteIntent::Read).await;
    let idle = native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Idle).await;
    let closed = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            interrupt: false,
            expected_state_revision: idle.state_revision,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert_eq!(closed.lifecycle, pb::RunLifecycle::Closed as i32);
    assert_eq!(transcript_methods(&fixture, "turn/start").len(), 1);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_writer_release_and_reacquire() {
    // Writer release deliberately performs repeated executable-identity
    // hashing. Keep this isolated test under its wrapper deadline without
    // weakening that production census by stripping only the fixture copy.
    let fixture = Fixture::new_compact("run_start_model_list.json");
    let id = fixture.start_run(&["--execution-lane", "dedicated"]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut writers = pb::writer_service_client::WriterServiceClient::new(channel);
    let initial = native_snapshot(&mut runs, &fixture, &id).await;
    let first = native_submit(&mut runs, &fixture, &initial, pb::WriteIntent::Write).await;
    let first_generation = first.writer.unwrap().writer_generation;
    let idle = native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Idle).await;
    let released = writers
        .release_writer(pb::ReleaseWriterRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            expected_state_revision: idle.state_revision,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        released.authority_state,
        pb::WriterAuthorityState::None as i32
    );
    assert!(released.owner_run_id.is_none());
    assert_eq!(
        released.effective_access,
        pb::EffectiveAccess::Unknown as i32
    );
    let current = native_snapshot(&mut runs, &fixture, &id).await;
    assert_eq!(
        current.writer_authority.as_ref().unwrap().state,
        pb::WriterAuthorityState::None as i32
    );
    let second = native_submit(&mut runs, &fixture, &current, pb::WriteIntent::Write).await;
    let writer = second.writer.unwrap();
    assert_eq!(
        writer.authority_state,
        pb::WriterAuthorityState::Active as i32
    );
    assert_eq!(writer.owner_run_id.as_deref(), Some(id.as_str()));
    assert!(writer.writer_generation > first_generation);
    let idle = native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Idle).await;
    assert_eq!(transcript_methods(&fixture, "turn/start").len(), 2);
    let closed = runs
        .close_run(pb::CloseRunRequest {
            context: context(),
            run: fixture.run_ref(&id),
            controller: fixture.carrier(),
            interrupt: false,
            expected_state_revision: idle.state_revision,
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap();
    assert_eq!(closed.lifecycle, pb::RunLifecycle::Closed as i32);
    gateway.terminate();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_killed_worker_recovery() {
    for recover in [true, false] {
        let fixture = Fixture::with_scenario(active_turn_scenario(false));
        let id = fixture.start_run(&[]);
        let mut gateway = fixture.start_gateway();
        let mut runs = pb::run_service_client::RunServiceClient::new(gateway.channel().await);
        let initial = native_snapshot(&mut runs, &fixture, &id).await;
        native_submit(&mut runs, &fixture, &initial, pb::WriteIntent::Read).await;
        let active =
            native_wait_lifecycle(&mut runs, &fixture, &id, pb::RunLifecycle::Running).await;
        let runtime_path = dolgorae::worker::runtime_record_path(
            &dolgorae::worker::runtime_root(&fixture.state_root),
            Uuid::parse_str(&id).unwrap(),
        )
        .unwrap();
        let before: dolgorae::worker::WorkerRuntimeRecord =
            serde_json::from_slice(&fs::read(&runtime_path).unwrap()).unwrap();
        let exit = dolgorae::darwin::DarwinSystem
            .watch_process_exit(before.identity.pid)
            .unwrap();
        assert!(
            std::process::Command::new("/bin/kill")
                .args(["-KILL", &before.identity.pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !exit.exited().unwrap() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("killed worker remained alive");
        let result = if recover {
            runs.recover_run(pb::RecoverRunRequest {
                context: context(),
                run: fixture.run_ref(&id),
                controller: fixture.carrier(),
                expected_state_revision: active.state_revision,
            })
            .await
        } else {
            runs.reconcile_run(pb::ReconcileRunRequest {
                context: context(),
                run: fixture.run_ref(&id),
                controller: fixture.carrier(),
                expected_state_revision: active.state_revision,
            })
            .await
        }
        .unwrap_or_else(|error| {
            panic!(
                "recovery={recover} expected={} failed: {error}; audit={}",
                active.state_revision,
                audit_summary(&fixture, &id)
            )
        })
        .into_inner()
        .run
        .unwrap();
        assert_eq!(result.lifecycle, pb::RunLifecycle::Paused as i32);
        assert!(result.active_turn.is_none());
        assert_eq!(
            transcript_methods(&fixture, "turn/start").len(),
            1,
            "recovery must not execute another Turn"
        );
        assert!(transcript_methods(&fixture, "thread/read").len() >= 2);
        assert_eq!(turn_starts(&fixture, &id), 1);
        gateway.terminate();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_watch_input_rejection() {
    let fixture = Fixture::new("run_start_model_list.json");
    let id = fixture.start_run(&[]);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let mut observations = pb::observation_service_client::ObservationServiceClient::new(channel);
    let snapshot = native_snapshot(&mut runs, &fixture, &id).await;
    let before = audit(&fixture, &id);
    for (cursor, projection, version, code) in [
        (
            "01".to_owned(),
            pb::ProjectionProfile::Minimal as i32,
            1,
            "EVENT_CURSOR_INVALID",
        ),
        (
            (snapshot.state_revision + 1).to_string(),
            pb::ProjectionProfile::Minimal as i32,
            1,
            "EVENT_CURSOR_INVALID",
        ),
        (
            "0".to_owned(),
            pb::ProjectionProfile::Minimal as i32,
            2,
            "UNSUPPORTED_SCHEMA_VERSION",
        ),
        ("0".to_owned(), 9000, 1, "UNSUPPORTED_SCHEMA_VERSION"),
    ] {
        let response = observations
            .watch_run_events(pb::WatchRunEventsRequest {
                context: context(),
                run: fixture.run_ref(&id),
                after_cursor: cursor,
                projection,
                projection_version: version,
            })
            .await;
        let error = match response {
            Err(error) => error,
            Ok(response) => response.into_inner().message().await.unwrap_err(),
        };
        semantic_error(&error, code);
        assert_eq!(audit(&fixture, &id), before);
    }
    gateway.terminate();
}
