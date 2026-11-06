//! Client and independent observations for ordinary-binary native acceptance.
//! Product lifecycle changes are RPCs to the selected ordinary server.
#![deny(unsafe_code)]

use dolgorae::{
    controller::{CredentialCarrier, binding_from_carrier},
    darwin::DarwinSystem,
    delegation_identity::{AcceptedTask, BytesRef},
    gateway_projection_v2 as project,
    global_runtime::ResolvedGlobalProfile,
    instructions::{InstructionSourceStore, SelectedInstructionSource, SelectedNamedSource},
    ledger::{LedgerError, ObservedLedger},
    paths::DolgoraeHome,
    protocol::public_v2 as pb,
    provisioning::{
        ProvisioningAdmission, ProvisioningSources, RetainedSnapshot, capture_provisioning,
    },
    snapshot::RunSnapshot,
    worker::{self, ControlRequestV1, WorkerRuntimeRecord},
    workspace::WorkspaceService,
};
use prost::Message as _;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _},
    path::Path,
    process::Command,
};
use uuid::Uuid;

fn private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn save(root: &Path, name: &str, value: &impl serde::Serialize) {
    private(&root.join(name), &serde_json::to_vec(value).unwrap());
}

fn home(root: &Path) -> DolgoraeHome {
    let home = DolgoraeHome::system().unwrap();
    assert_eq!(home.root(), root.join("home/.dolgorae"));
    home
}

fn observe_runs(root: &Path) -> Value {
    let view = WorkspaceService::system()
        .unwrap()
        .discover(Some(&root.join("workspace")))
        .unwrap();
    let state = home(root).workspace_root(&view.workspace_id);
    let requested: Vec<Uuid> =
        serde_json::from_value(read(&root.join("ordinary-read-runs.json"))).unwrap();
    let observed: Vec<_> = requested.into_iter().map(|run| {
        let snapshot = RunSnapshot::load(&state, run, 0).unwrap();
        assert_eq!(snapshot.manifest.workspace_id, view.workspace_id);
        json!({"run_id":run,"state":snapshot.projection.lifecycle.as_str(),
            "active_turn_id":snapshot.projection.active_turn_id,"controller":snapshot.controller,
            "identity_verdict":snapshot.runtime_record.as_ref().map(|record| format!("{:?}", worker::classify_worker_identity(record))),
            "global_profile_binding":snapshot.manifest.global_profile_binding})
    }).collect();
    json!(observed)
}

fn capture(root: &Path) -> Value {
    let config = read(&root.join("input.json"));
    let home = home(root);
    let managed = config["scenario"] == "managed-producer";
    let writable = managed
        || matches!(
            config["scenario"].as_str(),
            Some("consumer-complete" | "consumer-rounds" | "consumer-release" | "consumer-fences")
        );
    // This performs native version/schema observations, but neither records a
    // binding nor starts a server. The ordinary StartRun RPC owns both effects.
    let binding = ResolvedGlobalProfile::resolve(
        &home,
        if managed {
            "integrated-successor-members"
        } else {
            "host-fixture"
        },
    )
    .unwrap()
    .prepare_specialist(&home, false)
    .unwrap();
    let admission = ProvisioningAdmission {
        model: config["executor_model"].as_str().unwrap().into(),
        effort: "low".into(),
        purpose_kind: if writable {
            "implementation"
        } else {
            "research"
        }
        .into(),
        purpose_label: None,
        required_capabilities: vec![],
        execution_lane: if writable {
            "dedicated"
        } else {
            "shared_readonly"
        }
        .into(),
        requested_access: if writable {
            "canonical_workspace_write"
        } else {
            "read_only"
        }
        .into(),
        required_assurance: "best_effort_personal_alpha".into(),
        native_subagent_policy: "disabled".into(),
    };
    let role = SelectedInstructionSource::File(vec!["roles".into(), "general".into()]);
    let dependencies = vec![SelectedNamedSource {
        name: if managed { "producer-delivery" } else { "original-evidence" }.into(),
        source: SelectedInstructionSource::Inline(
            if managed { "Keep the complete accepted task, native write output and remaining gaps separate from any consumer decision." } else { "Retain every declared source identity and complete result component; previews do not replace original bytes." }.into(),
        ),
    }];
    let selected_guidance = if managed {
        vec![]
    } else {
        vec![SelectedNamedSource {
            name: "producer-boundary".into(),
            source: SelectedInstructionSource::Inline(
                "A producer completion is a factual delivery, not semantic or user acceptance."
                    .into(),
            ),
        }]
    };
    let snapshot = capture_provisioning(&ProvisioningSources {
        instruct: &InstructionSourceStore::new(&home),
        role_name: "general",
        role: role.clone(),
        dependencies: dependencies.clone(),
        selected_guidance: selected_guidance.clone(),
        profile_binding: binding.clone(),
        admission: admission.clone(),
    })
    .unwrap();
    save(root, "ordinary-captured.json", &snapshot);
    save(root, "qualified-binding.json", &binding);
    let selection = json!({"role_name":"general","role":role,
        "dependencies":dependencies,"selected_guidance":selected_guidance,
        "profile_binding":binding,"admission":admission});
    save(root, "ordinary-provisioning-selection.json", &selection);
    json!({"selection":selection,"sources":snapshot.sources_identity().unwrap(),
        "candidate":snapshot.candidate(false).unwrap(),
        "observation":"unrecorded Profile preparation and input capture only"})
}

fn observe_mcp(root: &Path) -> Value {
    let member: Uuid =
        serde_json::from_value(read(&root.join("ordinary-mcp-member.json"))).unwrap();
    let view = WorkspaceService::system()
        .unwrap()
        .discover(Some(&root.join("workspace")))
        .unwrap();
    let state = home(root).workspace_root(&view.workspace_id);
    let snapshot = RunSnapshot::load(&state, member, 0).unwrap();
    let socket = worker::dedicated_server_socket_path(
        dolgorae::workspace::WorkspacePlatform::current_uid(
            &dolgorae::workspace::SystemWorkspacePlatform,
        ),
        member,
        snapshot.projection.run_generation,
    )
    .unwrap();
    let mut connection = dolgorae::app_server::JsonRpcConnection::connect(
        &socket,
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    connection.request("initialize", json!({"clientInfo":{"name":"ordinary-mcp-inventory","version":"1"},"capabilities":{"experimentalApi":true}})).unwrap();
    connection.notify("initialized", json!({})).unwrap();
    let inventory = connection.request("mcpServerStatus/list", json!({"threadId":snapshot.projection.thread_id,"serverName":"fixture","detail":"toolsAndAuthOnly"})).unwrap();
    save(root, "ordinary-mcp-inventory.json", &inventory);
    inventory
}

fn context() -> Option<pb::RequestContext> {
    Some(pb::RequestContext {
        protocol_version: 2,
        client_request_id: Uuid::now_v7().to_string(),
        client_instance_id: "task087-bootstrap".into(),
    })
}

async fn bootstrap(root: &Path, close: bool) -> Value {
    let home = home(root);
    let view = WorkspaceService::system()
        .unwrap()
        .discover(Some(&root.join("workspace")))
        .unwrap();
    let workspace = pb::WorkspaceRef {
        absolute_path: root.join("workspace").to_str().unwrap().into(),
        expected_workspace_id: view.workspace_id,
    };
    let socket = root.join("gateway.sock");
    let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            let socket = socket.clone();
            async move {
                tokio::net::UnixStream::connect(socket)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .unwrap();
    let mut client = pb::run_service_client::RunServiceClient::new(channel);
    let carrier_path = home
        .root()
        .join("controller-carriers/task087/bootstrap/credential");
    let carrier = CredentialCarrier::open_path(&carrier_path).unwrap();
    let binding = binding_from_carrier(&carrier, 1).unwrap();
    let controller = pb::ControllerCarrierRef {
        absolute_file_path: carrier_path.to_str().unwrap().into(),
        expected_controller_id: binding.identity.controller_id.to_string(),
        expected_controller_generation: 1,
    };
    if !close {
        let captured: RetainedSnapshot =
            serde_json::from_slice(&fs::read(root.join("ordinary-captured.json")).unwrap())
                .unwrap();
        let (instructions, _) = captured.admitted_configuration().unwrap();
        let scenario = read(&root.join("input.json"));
        let writable = matches!(
            scenario["scenario"].as_str(),
            Some(
                "managed-producer"
                    | "consumer-complete"
                    | "consumer-rounds"
                    | "consumer-release"
                    | "consumer-fences"
            )
        );
        let response = client
            .start_run(pb::StartRunRequest {
                context: context(),
                workspace: Some(workspace),
                controller: Some(controller),
                idempotency_key: "ordinary-profile-bootstrap".into(),
                provisioning: Some(pb::ProvisioningSelection {
                    profile_name: captured.profile.clone(),
                    role_name: captured.role_name().into(),
                    policy_name: String::new(),
                    policy_snapshot_digest: String::new(),
                    configuration: Some(
                        project::admitted_configuration(&captured.admitted).unwrap(),
                    ),
                    agent_configuration_digest: captured.admitted.digest().unwrap(),
                }),
                requested_mode: if writable {
                    pb::CreationMode::Writable
                } else {
                    pb::CreationMode::ReadOnly
                } as i32,
                execution_lane: if writable {
                    pb::ExecutionLane::Dedicated
                } else {
                    pb::ExecutionLane::SharedReadonly
                } as i32,
                purpose: if writable {
                    pb::PurposeKind::Implementation
                } else {
                    pb::PurposeKind::Research
                } as i32,
                purpose_label: None,
                model: Some(captured.admitted.model.clone()),
                effort: Some(captured.admitted.effort.clone()),
                required_assurance: pb::AssuranceLevel::BestEffortPersonalAlpha as i32,
                required_capabilities: vec![],
                instructions: Some(project::captured_instructions(&instructions)),
                parent: None,
            })
            .await
            .unwrap()
            .into_inner();
        private(
            &root.join("ordinary-bootstrap-start.pb"),
            &response.encode_to_vec(),
        );
        let run = response.run.as_ref().unwrap();
        assert_eq!(run.lifecycle, pb::RunLifecycle::Idle as i32);
        assert!(run.active_turn.is_none());
        let metadata = json!({"run_id":run.run_id,"state_revision":run.state_revision,
            "exact_replay":response.exact_replay,"active_turn":null,"lifecycle":"idle",
            "full_response":"ordinary-bootstrap-start.pb"});
        save(root, "ordinary-bootstrap.json", &metadata);
        metadata
    } else {
        let started = read(&root.join("ordinary-bootstrap.json"));
        let reference = pb::RunRef {
            workspace: Some(workspace),
            run_id: started["run_id"].as_str().unwrap().into(),
        };
        let observed = client
            .get_run(pb::GetRunRequest {
                context: context(),
                run: Some(reference.clone()),
            })
            .await
            .unwrap()
            .into_inner();
        private(
            &root.join("ordinary-bootstrap-before-close.pb"),
            &observed.encode_to_vec(),
        );
        let run = observed.run.unwrap();
        assert!(run.active_turn.is_none());
        let response = client
            .close_run(pb::CloseRunRequest {
                context: context(),
                run: Some(reference),
                controller: Some(controller),
                interrupt: false,
                expected_state_revision: run.state_revision,
            })
            .await
            .unwrap()
            .into_inner();
        private(
            &root.join("ordinary-bootstrap-close.pb"),
            &response.encode_to_vec(),
        );
        let run = response.run.unwrap();
        assert_eq!(run.lifecycle, pb::RunLifecycle::Closed as i32);
        json!({"run_id":run.run_id,"state_revision":run.state_revision,
            "lifecycle":"closed","full_response":"ordinary-bootstrap-close.pb"})
    }
}

fn checked_worker(root: &Path) -> WorkerRuntimeRecord {
    let home = home(root);
    let selected = read(&root.join("fault/selected-request.json"));
    let request: ControlRequestV1 = serde_json::from_value(selected).unwrap();
    let ControlRequestV1::ExternalSubmit {
        expected,
        engagement_id,
        request,
        ..
    } = request
    else {
        panic!("selected request is not ExternalSubmit")
    };
    expected.validate().unwrap();
    let match_engagement = fs::read_to_string(root.join("fault/engagement.match")).unwrap();
    assert_eq!(match_engagement.trim(), engagement_id.to_string());
    assert!(request.message.contains("TASK086_GROUP_SOURCE_1"));
    let group = read(&root.join("ordinary-group.json"));
    assert_eq!(
        request.idempotency_key,
        format!(
            "group:{}:1:assign",
            group["facts"]["group_ref"].as_str().unwrap()
        )
    );
    let state = home.workspace_root(&expected.workspace_id);
    let view = WorkspaceService::system()
        .unwrap()
        .discover(Some(&root.join("workspace")))
        .unwrap();
    assert_eq!(expected.workspace_id, view.workspace_id);
    assert_eq!(expected.uid, DarwinSystem.current_uid());
    let record = worker::read_runtime_record(
        &worker::runtime_record_path(&worker::runtime_root(&state), expected.run_id).unwrap(),
        expected.uid,
    )
    .unwrap();
    assert_eq!(record.identity, expected);
    let snapshot = RunSnapshot::load(&state, expected.run_id, record.control_socket_epoch).unwrap();
    assert_eq!(snapshot.projection.run_generation, expected.run_generation);
    assert_eq!(
        snapshot.manifest.aggregate_binding.unwrap().aggregate_id,
        engagement_id
    );
    let config = read(&root.join("input.json"));
    let image = fs::canonicalize(config["binary"].as_str().unwrap()).unwrap();
    let metadata = fs::metadata(&image).unwrap();
    assert_eq!(expected.executable_path, image);
    assert_eq!(
        (expected.executable_device, expected.executable_inode),
        (metadata.dev(), metadata.ino())
    );
    let digest = dolgorae::jcs::sha256_hex(&fs::read(&image).unwrap());
    assert_eq!(expected.executable_sha256, digest);
    assert_eq!(record.binary_sha256, digest);
    assert_eq!(DarwinSystem.live_process_path(expected.pid).unwrap(), image);
    assert_eq!(
        worker::classify_worker_identity(&record),
        worker::ProcessIdentityVerdict::Match
    );
    let live = DarwinSystem.bsd_process_identity(expected.pid).unwrap();
    assert!(!live.zombie);
    assert!(
        root.join(format!("fault/loaded-{}", expected.pid))
            .is_file()
    );
    assert_eq!(
        (
            live.uid,
            live.process_group_id,
            live.session_id,
            live.start_tvsec,
            live.start_tvusec
        ),
        (
            expected.uid,
            expected.process_group_id,
            expected.session_id,
            expected.start_tvsec,
            expected.start_tvusec
        )
    );
    let socket = fs::symlink_metadata(&record.socket_path).unwrap();
    assert!(socket.file_type().is_socket());
    assert_eq!(socket.uid(), expected.uid);
    assert_eq!(
        (socket.dev(), socket.ino()),
        (record.socket_identity.device, record.socket_identity.inode)
    );
    let selected_socket: Vec<u64> =
        fs::read_to_string(root.join("fault/selected-request.socket-identity"))
            .unwrap()
            .split_whitespace()
            .map(|part| part.parse().unwrap())
            .collect();
    assert_eq!(selected_socket, [socket.dev(), socket.ino()]);
    assert_eq!(
        fs::read_to_string(root.join("fault/selected-request.socket")).unwrap(),
        record.socket_path.to_str().unwrap()
    );
    record
}

fn serve_identity(root: &Path) -> Value {
    let pid: u32 = fs::read_to_string(root.join("fault/serve.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let live = DarwinSystem.bsd_process_identity(pid).unwrap();
    assert!(!live.zombie && live.uid == DarwinSystem.current_uid());
    let config = read(&root.join("input.json"));
    assert_eq!(
        DarwinSystem.live_process_path(pid).unwrap(),
        fs::canonicalize(config["binary"].as_str().unwrap()).unwrap()
    );
    assert!(root.join(format!("fault/loaded-{pid}")).is_file());
    private(
        &root.join("fault/serve.identity"),
        format!(
            "{pid} {} {} {}\n",
            live.uid, live.start_tvsec, live.start_tvusec
        )
        .as_bytes(),
    );
    json!({"serve":live,"image":config["binary"]})
}

fn select_worker(root: &Path) -> Value {
    let record = checked_worker(root);
    let expected = &record.identity;
    let fault = root.join("fault");
    private(
        &fault.join("worker.identity"),
        format!(
            "{} {} {} {}\n",
            expected.pid, expected.uid, expected.start_tvsec, expected.start_tvusec
        )
        .as_bytes(),
    );
    private(
        &fault.join("control.socket"),
        record.socket_path.to_str().unwrap().as_bytes(),
    );
    private(
        &fault.join("control.identity"),
        format!(
            "{} {}\n",
            record.socket_identity.device, record.socket_identity.inode
        )
        .as_bytes(),
    );
    private(
        &fault.join("lifecycle.lock"),
        home(root)
            .workspace_root(&expected.workspace_id)
            .join("runtime/locks/lifecycle")
            .join(format!("{}.lock", expected.run_id))
            .to_str()
            .unwrap()
            .as_bytes(),
    );
    save(root, "ordinary-selected-worker.json", &record);
    json!({"record":record,"selection":"typed ExternalSubmit, owned live generation, image and socket verified"})
}

fn group_inputs(root: &Path) -> Value {
    let config = read(&root.join("input.json"));
    let host = root.join(config["host"].as_str().unwrap());
    let opened = read(&root.join("ordinary-open.json"));
    let scope = &opened["facts"]["engagement"]["engagement_ref"];
    let installed = read(&root.join("ordinary-install.json"));
    let mut entries = Vec::new();
    let mut inputs = Vec::new();
    for index in 0..2 {
        let accepted: AcceptedTask =
            serde_json::from_value(read(&root.join(format!("ordinary-accepted-{index}.json"))))
                .unwrap();
        let selection = read(&host.join(format!("ordinary-task-{index}.json")));
        let bytes = dolgorae::jcs::canonicalize(&dolgorae::jcs::parse(&serde_json::to_string(&json!({
            "schema":"dolgorae.group-task-input/v1", "provisioning_ref":installed["facts"]["candidate"]["provisioning_ref"],
            "assignment":{"task_body":selection["task"],"accepted_task_identity":accepted,"deadline_seconds":60}
        })).unwrap()).unwrap()).unwrap();
        let path = root.join("workspace").join(format!("source-{index}.json"));
        private(&path, &bytes);
        inputs.push(json!({"entry_key":format!("source-{index}"),"absolute_file_path":path,"content":BytesRef::from_bytes(&bytes)}));
        entries.push(json!({"entry_key":format!("source-{index}"),"role_snapshot_digest":accepted.role_snapshot_digest,
            "agent_configuration_digest":accepted.agent_configuration_digest,"accepted_task_digest":accepted.digest().unwrap()}));
    }
    let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"assign_group",
        "scope_kind":"external_engagement","scope_ref":scope,"idempotency_key":"fixture-group","expected_revision":null,
        "manifest":{"schema":"dolgorae.group-assignment/v1","scope_kind":"external_engagement","scope_ref":scope,"entries":entries},"inputs":inputs});
    save(root, "ordinary-group-request.json", &request);
    request
}

fn observe_images(root: &Path) -> Value {
    let config = read(&root.join("input.json"));
    let home = home(root);
    let view = WorkspaceService::system()
        .unwrap()
        .discover(Some(&root.join("workspace")))
        .unwrap();
    let state = home.workspace_root(&view.workspace_id);
    let image = fs::canonicalize(config["binary"].as_str().unwrap()).unwrap();
    let metadata = fs::metadata(&image).unwrap();
    let digest = dolgorae::jcs::sha256_hex(&fs::read(&image).unwrap());
    let assigned = read(&root.join("assigned.json"));
    let managed = config["scenario"] == "managed-producer";
    let mut runs = std::collections::BTreeSet::new();
    runs.insert(assigned["member_ref"].as_str().unwrap().to_owned());
    if managed {
        runs.insert(assigned["scope_ref"].as_str().unwrap().to_owned());
    } else if config["scenario"]
        .as_str()
        .unwrap()
        .starts_with("consumer-group")
    {
        let coverage = read(&root.join("ordinary-recovered-coverage.json"));
        for source in coverage["sources"].as_array().unwrap() {
            runs.insert(source["worker_ref"].as_str().unwrap().to_owned());
        }
    }
    let mut workers = Vec::new();
    let mut profile_keys = std::collections::BTreeSet::new();
    for run in runs {
        let manifest = read(&state.join("runs").join(&run).join("manifest.json"));
        profile_keys.insert(
            manifest["global_profile_binding"]["server_key"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        let run = Uuid::parse_str(&run).unwrap();
        let record = worker::read_runtime_record(
            &worker::runtime_record_path(&worker::runtime_root(&state), run).unwrap(),
            DarwinSystem.current_uid(),
        )
        .unwrap();
        let identity = &record.identity;
        assert_eq!(identity.run_id, run);
        assert_eq!(identity.workspace_id, view.workspace_id);
        assert_eq!(identity.executable_path, image);
        assert_eq!(
            (identity.executable_device, identity.executable_inode),
            (metadata.dev(), metadata.ino())
        );
        assert_eq!(identity.executable_sha256, digest);
        assert_eq!(record.binary_sha256, digest);
        assert_eq!(
            worker::classify_worker_identity(&record),
            worker::ProcessIdentityVerdict::Match
        );
        assert_eq!(DarwinSystem.live_process_path(identity.pid).unwrap(), image);
        if managed {
            let dedicated = record.dedicated_server_identity.as_ref().unwrap();
            let codex =
                fs::canonicalize(config["executor_identity"]["path"].as_str().unwrap()).unwrap();
            assert_eq!(
                worker::classify_dedicated_server_identity(dedicated),
                worker::ProcessIdentityVerdict::Match
            );
            assert_eq!(dedicated.executable_path, codex);
            assert_eq!(
                dedicated.executable_sha256,
                config["executor_identity"]["sha256"].as_str().unwrap()
            );
            assert_eq!(
                DarwinSystem.live_process_path(dedicated.pid).unwrap(),
                codex
            );
        }
        workers.push(record);
    }
    let mut profiles = Vec::new();
    for server_key in profile_keys {
        let server: dolgorae::profile::ServerState = serde_json::from_value(read(
            &home
                .root()
                .join("profiles")
                .join(&server_key)
                .join("state.json"),
        ))
        .unwrap();
        assert_eq!(server.server_key, server_key);
        let drainer = DarwinSystem
            .live_process_identity(server.drainer_pid)
            .unwrap();
        assert_eq!(
            (drainer.uid, drainer.process_group_id),
            (server.drainer_uid, server.drainer_pgid)
        );
        assert_eq!(drainer.fingerprint, server.drainer_fingerprint);
        assert_eq!(
            DarwinSystem.live_process_path(server.drainer_pid).unwrap(),
            image
        );
        let provider = DarwinSystem.live_process_identity(server.pid).unwrap();
        assert_eq!(
            (provider.uid, provider.process_group_id),
            (server.uid, server.pgid)
        );
        assert_eq!(provider.fingerprint, server.process_fingerprint);
        let codex =
            fs::canonicalize(config["executor_identity"]["path"].as_str().unwrap()).unwrap();
        assert_eq!(DarwinSystem.live_process_path(server.pid).unwrap(), codex);
        assert_eq!(
            dolgorae::jcs::sha256_hex(&fs::read(codex).unwrap()),
            config["executor_identity"]["sha256"].as_str().unwrap()
        );
        profiles.push(json!({"profile_state":server,
        "drainer":{"fingerprint":drainer.fingerprint,"bsd":DarwinSystem.bsd_process_identity(server.drainer_pid).unwrap()},
        "provider":{"fingerprint":provider.fingerprint,"bsd":DarwinSystem.bsd_process_identity(server.pid).unwrap()}}));
    }
    let result = json!({"ordinary_image":{"path":image,"sha256":digest,"device":metadata.dev(),"inode":metadata.ino()},"workers":workers,"profiles":profiles});
    save(root, "ordinary-live-images.json", &result);
    result
}

fn terminal(root: &Path) -> Value {
    let record: WorkerRuntimeRecord =
        serde_json::from_value(read(&root.join("ordinary-selected-worker.json"))).unwrap();
    let state = home(root).workspace_root(&record.identity.workspace_id);
    let snapshot =
        match RunSnapshot::load(&state, record.identity.run_id, record.control_socket_epoch) {
            Ok(snapshot) => snapshot,
            Err(error) if error.code == "RUN_STATE_CONFLICT" => {
                return json!({"observation":"capture_conflict","last_terminal":null});
            }
            Err(error) => panic!("terminal snapshot: {error:?}"),
        };
    let ledger = match ObservedLedger::open_complete(
        &state.join("runs").join(record.identity.run_id.to_string()),
        record.identity.run_id,
        snapshot.projection.ledger_head.sequence,
    ) {
        Ok(ledger) => ledger,
        Err(LedgerError::CaptureConflict) => {
            return json!({"observation":"capture_conflict","last_terminal":null});
        }
        Err(error) => panic!("terminal ledger: {error:?}"),
    };
    json!({"last_terminal":ledger.last_terminal().unwrap(),"projection":snapshot.projection})
}

fn fault_worker(root: &Path) -> Value {
    let held: worker::ControlResponseV1 =
        serde_json::from_value(read(&root.join("fault/accepted.frame"))).unwrap();
    let worker::ControlResponseV1::Accepted { accepted } = held else {
        panic!("genuine Accepted frame missing")
    };
    let observed = terminal(root);
    if observed["observation"] == "capture_conflict" {
        return json!({"observation":"capture_conflict","fault_injected":false});
    }
    let terminal = &observed["last_terminal"];
    assert_eq!(terminal["thread_id"], accepted.thread_id);
    assert_eq!(terminal["turn_id"], accepted.turn_id);
    assert_eq!(terminal["status"], "completed");
    assert!(!root.join("fault/release-ack").exists());
    assert!(!root.join("fault/fail-safe-release").exists());
    let record = checked_worker(root);
    let previous: WorkerRuntimeRecord =
        serde_json::from_value(read(&root.join("ordinary-selected-worker.json"))).unwrap();
    assert_eq!(record, previous);
    // Revalidate the complete owned identity immediately before the external
    // physical fault. No lifecycle, receipt or task state is written here.
    let status = Command::new("/bin/kill")
        .args(["-KILL", &record.identity.pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let result = json!({"worker_identity":record.identity,"accepted":accepted,"fault_injected":true,
        "last_terminal":terminal,"fault":"physical SIGKILL with original Accepted held"});
    save(root, "owned-worker-fault.json", &result);
    result
}

#[tokio::main]
async fn main() {
    assert_eq!(
        std::env::var("DOLGORAE_RUN_NATIVE_PRODUCTION_HOST").as_deref(),
        Ok("1")
    );
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(args.len(), 3);
    let root = fs::canonicalize(&args[2]).unwrap();
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-production-host/v1\n"
    );
    let result = match args[1].to_str().unwrap() {
        "capture" => capture(&root),
        "bootstrap-start" => bootstrap(&root, false).await,
        "bootstrap-close" => bootstrap(&root, true).await,
        "select-worker" => select_worker(&root),
        "group-inputs" => group_inputs(&root),
        "accepted-task-digest" => {
            let accepted: AcceptedTask =
                serde_json::from_value(read(&root.join("ordinary-managed-accepted.json"))).unwrap();
            json!(accepted.digest().unwrap())
        }
        "observe-images" => observe_images(&root),
        "observe-mcp" => observe_mcp(&root),
        "observe-runs" => observe_runs(&root),
        "serve-identity" => serve_identity(&root),
        "terminal" => terminal(&root),
        "fault-worker" => fault_worker(&root),
        _ => panic!("unsupported ordinary client operation"),
    };
    println!("{result}");
}
