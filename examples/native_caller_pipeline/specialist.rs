//! Actual public StartRun and native tool use; no private owner/admin setup.
use super::*;

/// Failure cleanup asks the original native Primary to use its release owner.
pub(super) fn cleanup(
    root: &Path,
    home: &DolgoraeHome,
    view: &WorkspaceView,
    credential: &Path,
) -> Result<(), dolgorae::machine::MachineError> {
    let mut member = None;
    let mut released = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("native-specialist-raw-")
        {
            continue;
        }
        let records: Value = serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
        for record in records.as_array().unwrap() {
            for label in ["spawn", "release"] {
                if let Some(raw) = record["results"].get(label) {
                    let value: Value = match raw {
                        Value::String(text) => serde_json::from_str(text).unwrap(),
                        value => value.clone(),
                    };
                    if value["ok"] == true {
                        let facts = value["facts"]["member"].clone();
                        if label == "spawn" {
                            member = Some(facts);
                        } else if facts["state"] == "released" {
                            released.push(facts);
                        }
                    }
                }
            }
        }
    }
    let Some(member) = member else {
        return Ok(());
    };
    if released.iter().any(|facts| {
        facts["member_ref"] == member["member_ref"] && facts["scope_ref"] == member["scope_ref"]
    }) {
        return Ok(());
    }
    let session = member["scope_ref"].as_str().unwrap();
    let before = snapshot(home, view, session);
    if before.projection.lifecycle == RunLifecycle::Closed {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let socket = root.join("specialist-cleanup.sock");
    let gateway = Gateway::start(root, &socket);
    runtime.block_on(async {
        let connection = channel(&socket).await;
        let mut runs = RunServiceClient::new(connection);
        let run = Some(pb::RunRef {
            workspace: Some(pb::WorkspaceRef {
                absolute_path: view
                    .canonical_path
                    .to_path_buf()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
                expected_workspace_id: view.workspace_id.clone(),
            }),
            run_id: session.into(),
        });
        let carrier = CredentialCarrier::open_path(credential).unwrap();
        let controller = dolgorae::controller::binding_from_carrier(&carrier, 1).unwrap();
        let controller = Some(pb::ControllerCarrierRef {
            absolute_file_path: credential.to_str().unwrap().into(),
            expected_controller_id: controller.identity.controller_id.to_string(),
            expected_controller_generation: controller.identity.generation,
        });
        let current = get(&mut runs, &run).await;
        runs.submit_turn(pb::SubmitTurnRequest {
            context: context(),
            run: run.clone(),
            controller,
            idempotency_key: "native-fixture-cleanup".into(),
            write_intent: pb::WriteIntent::Read as i32,
            expected_state_revision: current.state_revision,
            message: format!(
                "NATIVE_CLEANUP ROOT={} MEMBER={} REV={}",
                session,
                member["member_ref"].as_str().unwrap(),
                member["revision"]
            ),
            ..Default::default()
        })
        .await
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let current = get(&mut runs, &run).await;
            if current.lifecycle == pb::RunLifecycle::Idle as i32
                && current
                    .last_final_response
                    .as_ref()
                    .and_then(|r| r.value.as_ref())
                    == Some(&pb::final_response::Value::InlineUtf8(
                        "NATIVE_CLEANUP final".into(),
                    ))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native cleanup requires original-state recovery"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    drop(gateway);
    Ok(())
}

fn bytes_ref(bytes: &[u8], identity: &pb::BytesRef) {
    assert_eq!(identity.byte_length, bytes.len() as u64);
    assert_eq!(
        identity.sha256,
        format!("sha256:{}", dolgorae::jcs::sha256_hex(bytes))
    );
}
pub(super) fn campaign(
    root: &Path,
    home: &DolgoraeHome,
    view: &WorkspaceView,
    _credential: &Path,
    request: pb::StartRunRequest,
) -> Value {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let socket = root.join("specialist.sock");
    let gateway = Gateway::start(root, &socket);
    let disabled = root.join("specialist-disabled").exists();
    let result = runtime.block_on(async {
        let connection = channel(&socket).await;
        let mut runs = RunServiceClient::new(connection.clone());
        let first = runs.start_run(request.clone()).await.unwrap().into_inner().run.unwrap();
        let replay = runs.start_run(request.clone()).await.unwrap().into_inner().run.unwrap();
        assert_eq!(first.run_id,replay.run_id);
        let run = Some(pb::RunRef { workspace: request.workspace.clone(), run_id:first.run_id.clone() });
        let initial = snapshot(home,view,&first.run_id);
        assert_eq!(initial.manifest.schema_version,4);
        let controller = request.controller.clone();
        for case in if disabled { vec!["disabled"] } else { vec!["inline","artifact"] } {
            let current = get(&mut runs,&run).await;
            runs.submit_turn(pb::SubmitTurnRequest {
                context: context(),run:run.clone(),controller:controller.clone(),
                idempotency_key:format!("specialist-{case}"),
                write_intent:pb::WriteIntent::Read as i32,
                expected_state_revision:current.state_revision,
                message:format!("NATIVE_PRIMARY {case} ROOT={}",first.run_id),..Default::default()
            }).await.unwrap();
            let deadline = Instant::now()+Duration::from_secs(180);
            loop {
                let current=get(&mut runs,&run).await;
                if current.lifecycle == pb::RunLifecycle::Idle as i32
                    && current.last_final_response.as_ref().and_then(|r|r.value.as_ref()) == Some(&pb::final_response::Value::InlineUtf8(format!("NATIVE_PRIMARY {case} final"))) { break; }
                assert!(Instant::now()<deadline,"native specialist campaign did not settle");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if disabled { continue; }
            let facts:Value=serde_json::from_slice(&fs::read(root.join(format!("specialist-{case}.json"))).unwrap()).unwrap();
            let task_id=facts["task_ref"].as_str().unwrap().to_owned();
            let scope=Some(pb::ManagedScope { root_primary_run:run.clone(),session_id:first.run_id.clone() });
            let mut broker=pb::broker_service_client::BrokerServiceClient::new(connection.clone());
            let collected=broker.collect_specialist_task(pb::CollectSpecialistTaskRequest {
                context:context(),scope,controller:controller.clone(),task_id:task_id.clone()
            }).await.unwrap().into_inner();
            let task=collected.task.unwrap();
            assert_eq!(task.task_id,task_id);
            let complete=task.result.unwrap();
            let expected=if case=="inline" {"NATIVE_SPECIALIST_INLINE".to_owned()} else {format!("NATIVE_SPECIALIST_ARTIFACT:{}","λ".repeat(600_000))}.into_bytes();
            let identity=complete.identity.unwrap().final_response.unwrap();
            bytes_ref(&expected,&identity);
            let content=complete.final_response.unwrap();
            bytes_ref(&expected,content.content.as_ref().unwrap());
            let received=match content.representation.unwrap() {
                pb::content_ref::Representation::InlineBytes(bytes)=>{ assert_eq!(case,"inline");bytes }
                pb::content_ref::Representation::Artifact(artifact)=>{
                    assert_eq!(case,"artifact");
                    assert_eq!(artifact.byte_length,expected.len() as u64);
                    assert_eq!(artifact.sha256,dolgorae::jcs::sha256_hex(&expected));
                    let mut artifacts=pb::artifact_service_client::ArtifactServiceClient::new(connection.clone());
                    let mut bytes=Vec::new();
                    loop {
                        let chunk=artifacts.read_artifact_chunk(pb::ReadArtifactChunkRequest {
                            context:context(),run:content.artifact_owner.clone(),controller:controller.clone(),
                            artifact_id:artifact.artifact_id.clone(),offset:bytes.len() as u64,length:16384
                        }).await.unwrap().into_inner();
                        assert_eq!(chunk.offset,bytes.len() as u64);
                        bytes.extend_from_slice(&chunk.data);
                        assert!(bytes.len()<=expected.len());
                        if chunk.eof {break;}
                    }
                    bytes
                }
                other=>panic!("complete result unavailable: {other:?}"),
            };
            assert_eq!(received,expected);
            if case=="inline" {
                let generation=lose_idle_worker(root,home,view,&first.run_id,Duration::from_secs(15)).await;
                let current=get(&mut runs,&run).await;
                let recovered=runs.recover_run(pb::RecoverRunRequest {context:context(),run:run.clone(),controller:controller.clone(),expected_state_revision:current.state_revision}).await.unwrap().into_inner().run.unwrap();
                assert_eq!(recovered.thread.as_ref().unwrap().thread_id,initial.projection.thread_id.as_deref().unwrap());
                assert!(snapshot(home,view,&first.run_id).projection.run_generation>generation);
            }
        }
        let current=get(&mut runs,&run).await;
        let closed=runs.close_run(pb::CloseRunRequest {context:context(),run:run.clone(),controller,expected_state_revision:current.state_revision,interrupt:false}).await.unwrap().into_inner();
        assert_eq!(closed.run.unwrap().lifecycle,pb::RunLifecycle::Closed as i32);
        let after=snapshot(home,view,&first.run_id);
        assert!(after.writer.holder.is_none());
        assert!(after.runtime_record.as_ref().is_none_or(|runtime| DarwinSystem.bsd_process_identity(runtime.identity.pid).is_err()));
        json!({"schema":"dolgorae.native-caller-pipeline/v1","mode":"native_specialist","disabled_policy":disabled,
            "public_start_replay":true,"worker_restart_original_owner":!disabled,"public_full_result_collection":!disabled,"inline_and_artifact":!disabled,
            "no_private_owner_injection":true,"root_close_verified":true,"native_turns":if disabled {1}else{4}})
    });
    drop(gateway);
    result
}
