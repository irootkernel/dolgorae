//! TASK086-only protected-native group preparation and owner observations.
use super::*;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn read(root: &Path, name: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}
fn until(root: &Path) -> u64 {
    fs::read_to_string(root.join("deadline-unix-ms"))
        .unwrap()
        .parse()
        .unwrap()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn deadline(root: &Path) -> Instant {
    let left = until(root)
        .checked_sub(now())
        .expect("original TASK086 deadline exhausted");
    Instant::now() + Duration::from_millis(left)
}
fn native_receipt(
    root: &Path,
    owner: &VerifiedOwner,
    scope: &service::DelegationScope<'_>,
    phase: &str,
    operation: &str,
    key: &str,
) -> Value {
    let header = read(root, &format!("task086-native-{phase}.json"));
    assert_eq!(header["operation"], operation);
    assert_eq!(header["ok"], true);
    let receipt: crate::delegation_identity::OperationReceipt =
        serde_json::from_value(header["facts"]["receipt"].clone()).unwrap();
    let lookup = service::lookup_operation(
        scope,
        operation,
        Some((
            "external_engagement",
            receipt.namespace.scope_ref.as_deref().unwrap(),
        )),
        key,
    )
    .unwrap();
    assert_eq!(lookup["receipt"], header["facts"]["receipt"]);
    let store = crate::engagement::EngagementStore::open_read_only(
        &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
    )
    .unwrap();
    let context = crate::engagement::accepted_native_context(store.connection(), &receipt)
        .unwrap()
        .expect("actual protected native origin is required");
    context.validate().unwrap();
    json!({"lookup":lookup,"native_context":context})
}

pub fn prepare_group(
    root: &Path,
    owner: &VerifiedOwner,
    _port: &SemanticRunSettlementPort<'_>,
    engagement: Uuid,
    member: Uuid,
    candidate: &Value,
    base: &crate::task_request::SpecialistTaskRequest,
) -> Value {
    let scope = service::DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    };
    let configuration = service::preparation_configuration(
        &scope,
        service::SettlementOwner::External {
            engagement_id: engagement,
        },
        member,
    )
    .unwrap();
    let mut entries = Vec::new();
    let mut inputs = Vec::new();
    for index in 0..2 {
        let mut task = base.clone();
        task.purpose = "research".into();
        task.brief.push_str(&format!(
            "\nTASK086_GROUP_SOURCE_{index}: report an independent claim; preserve disagreements."
        ));
        task.expected_output = "structured_findings_v1".into();
        let (accepted, _) =
            crate::successor_capture::capture_facade_task(&configuration, &task, "read_only", None)
                .unwrap();
        let key = format!("source-{index}");
        let bytes = crate::operation::canonical(&json!({
            "schema":"dolgorae.group-task-input/v1","provisioning_ref":candidate["provisioning_ref"],
            "assignment":{"task_body":task,"accepted_task_identity":accepted,"deadline_seconds":60},
        })).unwrap();
        let path = root.join("workspace").join(format!("{key}.json"));
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        inputs.push(json!({"entry_key":key,"absolute_file_path":path,"content":crate::delegation_identity::BytesRef::from_bytes(&bytes)}));
        entries.push(json!({"entry_key":key,"role_snapshot_digest":accepted.role_snapshot_digest,
            "agent_configuration_digest":accepted.agent_configuration_digest,"accepted_task_digest":accepted.digest().unwrap()}));
    }
    let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"assign_group",
        "scope_kind":"external_engagement","scope_ref":engagement,"idempotency_key":"fixture-group","expected_revision":null,
        "manifest":{"schema":"dolgorae.group-assignment/v1","scope_kind":"external_engagement","scope_ref":engagement,"entries":entries},"inputs":inputs});
    let gap = input(root)["scenario"] == "consumer-group-gaps";
    let group_until = until(root).min(now() + if gap { 150_000 } else { 90_000 });
    if gap {
        crate::worker::native_ack_loss_write(&owner.state_root, "native-ack-arm.json",
            &json!({"schema":"native-ack-loss/v1","scope_ref":engagement,"manifest":request["manifest"],
                "accepted_task_digest":request["manifest"]["entries"][1]["accepted_task_digest"],
                "group_key":"fixture-group","until_ms":group_until})).unwrap();
    }
    let prepared = json!({"scope_ref":engagement,"member_ref":member,"candidate":candidate,
        "base_task":base,"group_request":request,"group_until_ms":group_until,"gap":gap});
    private(&root.join("task086-prepared.json"), &prepared);
    json!({"pending_native_group":true,"scope_ref":engagement,"member_ref":member})
}

pub fn control_plane(root: &Path) -> Value {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let config = input(root);
    assert!(
        config["scenario"]
            .as_str()
            .unwrap()
            .starts_with("consumer-group")
    );
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stopped);
    std::thread::spawn(move || {
        let mut command = String::new();
        let _ = std::io::stdin().read_line(&mut command);
        signal.store(true, Ordering::Release);
    });
    let home = DolgoraeHome::system().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let gateway =
        crate::gateway_socket::GatewaySocket::bind(&home, &root.join("gateway.sock")).unwrap();
    let control = {
        let _entered = runtime.enter();
        crate::scheduler::ControlPlaneRuntime::start(&home, gateway.record().server_instance_id)
            .unwrap()
    };
    private(
        &root.join("task086-control-ready.json"),
        &json!({"ready":true,"pid":std::process::id()}),
    );
    let end = deadline(root) + Duration::from_secs(30);
    while !stopped.load(Ordering::Acquire) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(25));
    }
    runtime.block_on(control.shutdown(
        tokio::time::Instant::now()
            + Duration::from_secs(5).min(end.saturating_duration_since(Instant::now())),
    ));
    drop(gateway);
    runtime.shutdown_timeout(
        Duration::from_secs(1).min(end.saturating_duration_since(Instant::now())),
    );
    json!({"started":true,"stopped":true})
}

pub fn unknown(root: &Path) -> Value {
    let prepared = read(root, "task086-prepared.json");
    assert_eq!(prepared["gap"], true);
    let home = DolgoraeHome::system().unwrap();
    let owner = VerifiedOwner::load(&home, &host_root(root).join("owner.json")).unwrap();
    let scope = service::DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    };
    let group_origin = native_receipt(
        root,
        &owner,
        &scope,
        "group",
        "assign_group",
        "fixture-group",
    );
    let coverage = read(root, "task086-native-await.json");
    assert_eq!(
        coverage["group_ref"],
        group_origin["lookup"]["accepted_subjects"]["group_ref"]
    );
    let rows = coverage["sources"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["entry_key"] == "source-0" && r["state"] == "completed")
    );
    let source = rows.iter().find(|r| r["entry_key"] == "source-1").unwrap();
    let end = deadline(root).min(
        Instant::now()
            + Duration::from_millis(
                prepared["group_until_ms"]
                    .as_u64()
                    .unwrap()
                    .saturating_sub(now()),
            ),
    );
    let held = loop {
        if let Some(held) =
            crate::worker::native_ack_loss_read(&owner.state_root, "native-ack-held.json").unwrap()
            && root.join("unknown-post-observed").is_file()
        {
            break held;
        }
        assert!(
            Instant::now() < end,
            "real source1 Accepted/HTTP barrier was not reached"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(held["task_ref"], source["task_ref"]);
    assert_eq!(held["run_id"], source["worker_ref"]);
    let engagement = Uuid::parse_str(prepared["scope_ref"].as_str().unwrap()).unwrap();
    let run = Uuid::parse_str(source["worker_ref"].as_str().unwrap()).unwrap();
    let turn = held["accepted"]["turn_id"].as_str().unwrap();
    fs::write(root.join("release-source1-response"), b"release\n").unwrap();
    loop {
        if let Some(terminal) =
            crate::semantic::resolved_terminal(&owner.state_root, run, turn).unwrap()
        {
            assert_eq!(terminal.status, "completed");
            assert_eq!(terminal.turn_id, turn);
            assert_eq!(terminal.thread_id, held["accepted"]["thread_id"]);
            private(
                &root.join("source1-real-terminal.json"),
                &json!({
                "task_ref":source["task_ref"],"run_id":run,"turn_id":turn,"status":terminal.status}),
            );
            break;
        }
        assert!(Instant::now() < end, "same source1 native terminal missing");
        std::thread::sleep(Duration::from_millis(10));
    }
    let expected: crate::worker::WorkerIdentity =
        serde_json::from_value(held["worker_identity"].clone()).unwrap();
    fault_owned_worker(root, &owner, engagement, source, &expected);
    let unknown_until = until(root).min(now() + 10_000);
    private(
        &root.join("task086-unknown-window.json"),
        &json!({"until_ms":unknown_until}),
    );
    loop {
        let lookup = service::lookup_operation(
            &scope,
            "assign_external_specialist_task",
            Some((
                "external_engagement",
                prepared["scope_ref"].as_str().unwrap(),
            )),
            held["receipt"]["namespace"]["idempotency_key"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        if original_operation_unknown(&lookup, source)
            && crate::worker::native_ack_loss_read(&owner.state_root, "native-claim-unknown.json")
                .unwrap()
                .is_some()
        {
            return json!({"namespace":lookup["namespace"],"source":source,"until_ms":unknown_until});
        }
        assert!(
            now() < unknown_until,
            "original source1 unknown claim missing"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn merged(root: &Path) -> Value {
    let prepared = read(root, "task086-prepared.json");
    let home = DolgoraeHome::system().unwrap();
    let owner = VerifiedOwner::load(&home, &host_root(root).join("owner.json")).unwrap();
    let carrier =
        CredentialCarrier::open_path(&host_root(root).join("external-controller")).unwrap();
    let port = SemanticRunSettlementPort { carrier: &carrier };
    let scope = service::DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    };
    let group = native_receipt(
        root,
        &owner,
        &scope,
        "group",
        "assign_group",
        "fixture-group",
    );
    let merge_origin = native_receipt(
        root,
        &owner,
        &scope,
        "merge",
        "merge_group_results",
        "fixture-merge",
    );
    let merged = read(root, "task086-native-merge.json")["facts"].clone();
    let collected = json!({"coverage":read(root,"task086-native-coverage.json")});
    assert_eq!(
        merged["merged_identity"]["sources"],
        collected["coverage"]["sources"]
    );
    assert_eq!(
        collected["coverage"]["group_ref"],
        group["lookup"]["accepted_subjects"]["group_ref"]
    );
    let engagement = Uuid::parse_str(prepared["scope_ref"].as_str().unwrap()).unwrap();
    let gap = prepared["gap"] == true;
    let end = deadline(root)
        .min(Instant::now() + Duration::from_secs(60))
        .min(
            Instant::now()
                + Duration::from_millis(
                    prepared["group_until_ms"]
                        .as_u64()
                        .unwrap()
                        .saturating_sub(now()),
                ),
        );
    if gap {
        assert!(root.join("direct-unknown-complete").is_file());
        let source = collected["coverage"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["entry_key"] == "source-1")
            .unwrap();
        assert_eq!(source["state"], "unknown");
        let held =
            crate::worker::native_ack_loss_read(&owner.state_root, "native-claim-unknown.json")
                .unwrap()
                .unwrap();
        let before = read(root, "original-operation-unknown-before.json");
        let after = read(root, "original-operation-unknown-after.json");
        assert!(
            original_operation_unknown(&before, source)
                && original_operation_unknown(&after, source)
        );
        crate::worker::native_ack_loss_write(
            &owner.state_root,
            "native-recovery-release.json",
            &held,
        )
        .unwrap();
        let run = Uuid::parse_str(source["worker_ref"].as_str().unwrap()).unwrap();
        let task = Uuid::parse_str(source["task_ref"].as_str().unwrap()).unwrap();
        loop {
            let (observed, _) = service::await_external_specialist_tasks(
                &scope,
                service::AwaitExternalSpecialistTasks {
                    engagement_id: engagement,
                    member_ref: run,
                    tasks: &[task],
                    predicate: service::AwaitPredicate::All,
                    timeout_seconds: 0,
                },
            )
            .unwrap();
            if observed[0].state == "completed" {
                break;
            }
            assert!(
                Instant::now() < end,
                "original source1 history did not settle"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        service::reconcile_external_member_task(&scope, &port, engagement, run, task).unwrap();
        let settled = service::collect_external_specialist_task(
            &scope,
            &port,
            service::CollectExternalSpecialistTask {
                engagement_id: engagement,
                member_ref: run,
                task_ref: task,
                adapter_id: "dolgorae.external/1",
                adapter_version: env!("CARGO_PKG_VERSION"),
            },
        )
        .unwrap();
        assert_eq!(settled.task.state, "completed");
        assert_eq!(settled.disposition.state, "idle");
        assert!(!settled.disposition.cleanup_pending);
        assert_eq!(
            settled.task.result_digest.as_ref(),
            Some(&settled.disposition.observed_after_result_digest)
        );
        assert!(
            [
                settled.disposition.running,
                settled.disposition.queued,
                settled.disposition.claimed,
                settled.disposition.dispatching,
                settled.disposition.unresolved
            ]
            .iter()
            .all(|n| *n == 0)
        );
        private(
            &root.join("original-ack-loss-recovery.json"),
            &json!({"task_ref":task,"member_ref":run,
            "task_state":settled.task.state,"result_digest":settled.task.result_digest,"worker_disposition":settled.disposition.state,
            "observed_after_result_digest":settled.disposition.observed_after_result_digest,"cleanup_pending":settled.disposition.cleanup_pending}),
        );
    }
    let merge = Uuid::parse_str(
        merged["receipt"]["accepted_subjects"]["merge_ref"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let public = service::research_merge::public_merged(&scope, merge).unwrap();
    let content = &public["merged_output"];
    let run = Uuid::parse_str(content["artifact_owner"]["run_id"].as_str().unwrap()).unwrap();
    let snapshot = crate::snapshot::RunSnapshot::load(&owner.state_root, run, 0).unwrap();
    let id = content["artifact"]["artifact_id"].as_str().unwrap();
    let meta = crate::artifact::metadata(&owner.state_root, &snapshot, id, Some(&carrier)).unwrap();
    assert_eq!(
        meta.artifact.byte_length,
        content["content"]["byte_length"].as_u64().unwrap()
    );
    let mut bytes = Vec::new();
    loop {
        let page = crate::artifact::chunk(
            &owner.state_root,
            &snapshot,
            id,
            (bytes.len() as u64, 1024),
            Some(&carrier),
        )
        .unwrap();
        assert_eq!(page.offset, bytes.len() as u64);
        bytes.extend(page.data);
        if page.eof {
            break;
        }
    }
    assert_eq!(
        json!(crate::delegation_identity::BytesRef::from_bytes(&bytes)),
        content["content"]
    );
    let output: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(output["coverage"], collected["coverage"]["sources"]);
    assert_eq!(output["groups"].as_array().unwrap().len(), 1);
    assert_eq!(output["groups"][0]["conflict"], !gap);
    assert_eq!(
        output["groups"][0]["variants"].as_array().unwrap().len(),
        if gap { 1 } else { 2 }
    );
    let synthesis_member = service::hire_external_specialist(
        &scope,
        &port,
        service::HireExternalSpecialist {
            engagement_id: engagement,
            provisioning_ref: prepared["candidate"]["provisioning_ref"].as_str().unwrap(),
            role_ref: "general",
            agent_configuration_digest: prepared["candidate"]["agent_configuration_digest"]
                .as_str()
                .unwrap(),
            idempotency_key: "fixture-synthesis-hire",
            expected_revision: None,
        },
    )
    .unwrap();
    let member = Uuid::parse_str(&synthesis_member.member.member_ref).unwrap();
    let configuration = service::preparation_configuration(
        &scope,
        service::SettlementOwner::External {
            engagement_id: engagement,
        },
        member,
    )
    .unwrap();
    let mut task: crate::task_request::SpecialistTaskRequest =
        serde_json::from_value(prepared["base_task"].clone()).unwrap();
    task.purpose = "research".into();
    task.brief.push_str("\nTASK086_SYNTHESIS: return the exact immutable merged input with every source link and unresolved conflict; no semantic resolution.");
    task.expected_output = "complete_result".into();
    let (accepted, _) =
        crate::successor_capture::capture_facade_task(&configuration, &task, "read_only", None)
            .unwrap();
    let synthesis = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"assign_synthesis","scope_kind":"external_engagement","scope_ref":engagement,"member_ref":member,"merged_findings_digest":merged["merged_digest"],"merged_input":merged["merged_identity"]["merged_output"],"assignment":{"task_body":task,"accepted_task_identity":accepted,"deadline_seconds":60},"idempotency_key":"fixture-synthesis","expected_revision":null});
    private(
        &root.join("group-merge-provenance.json"),
        &json!({
            "group":read(root,"task086-native-group.json")["facts"],"coverage":collected,
            "merge":merged,"public_artifact":public,"synthesis":null,"merged_body":output,
            "native_origins":{"group":group,"merge":merge_origin},
        }),
    );
    private(&root.join("task086-synthesis.json"), &synthesis);
    synthesis
}

pub fn finish(root: &Path) -> Value {
    let home = DolgoraeHome::system().unwrap();
    let owner = VerifiedOwner::load(&home, &host_root(root).join("owner.json")).unwrap();
    let scope = service::DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    };
    let origins = json!({
        "group":native_receipt(root,&owner,&scope,"group","assign_group","fixture-group"),
        "merge":native_receipt(root,&owner,&scope,"merge","merge_group_results","fixture-merge"),
        "synthesis":native_receipt(root,&owner,&scope,"synthesis","assign_synthesis","fixture-synthesis"),
    });
    let mut provenance = read(root, "group-merge-provenance.json");
    provenance["synthesis"] = read(root, "task086-native-synthesis.json")["facts"].clone();
    provenance["native_origins"] = origins;
    let group_ref = provenance["coverage"]["coverage"]["group_ref"]
        .as_str()
        .unwrap();
    let sources = provenance["coverage"]["coverage"]["sources"]
        .as_array()
        .unwrap();
    assert_eq!(sources.len(), 2);
    let scope_ref = provenance["group"]["receipt"]["namespace"]["scope_ref"]
        .as_str()
        .unwrap();
    let mut rounds = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let accepted = service::lookup_operation(
            &scope,
            "assign_external_specialist_task",
            Some(("external_engagement", scope_ref)),
            &format!("group:{group_ref}:{index}:assign"),
        )
        .unwrap();
        assert_eq!(accepted["state"], "accepted");
        assert_eq!(
            accepted["accepted_subjects"]["task_ref"],
            source["task_ref"]
        );
        assert_eq!(
            accepted["accepted_subjects"]["member_ref"],
            source["worker_ref"]
        );
        rounds.push(json!({"task_ref":source["task_ref"],"member_ref":source["worker_ref"]}));
    }
    let synthesis = &provenance["synthesis"];
    assert_eq!(
        synthesis["receipt"]["accepted_subjects"]["task_ref"],
        synthesis["task"]["task_ref"]
    );
    assert_eq!(
        synthesis["receipt"]["accepted_subjects"]["member_ref"],
        synthesis["task"]["member_ref"]
    );
    rounds.push(json!({"task_ref":synthesis["task"]["task_ref"],"member_ref":synthesis["task"]["member_ref"]}));
    assert_eq!(
        rounds
            .iter()
            .map(|r| r["task_ref"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    private(&root.join("accepted-rounds.json"), &json!(rounds));
    private(&root.join("group-merge-provenance.json"), &provenance);
    let observed = super::observe(root);
    assert_eq!(observed["task"]["state"], "completed");
    assert_eq!(observed["complete_result"]["changes"], json!([]));
    observed
}
