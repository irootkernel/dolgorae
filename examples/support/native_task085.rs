//! Bounded TASK085 cases over the existing production native-host fixture.
use super::*;
use std::ffi::OsString;
use std::time::Duration;

fn root_snapshot(owner: &VerifiedOwner, root: Uuid) -> Value {
    let snapshot = crate::snapshot::RunSnapshot::load(&owner.state_root, root, 0).unwrap();
    json!({"run_id":root,"runtime":snapshot.runtime_record,"thread_id":snapshot.projection.thread_id,
        "profile":snapshot.manifest.global_profile_binding,"active_turn":snapshot.projection.active_turn_id,
        "latest_turn":snapshot.projection.latest_turn_id})
}

fn accepted_task_count(
    store: &crate::engagement::EngagementStore,
    managed: bool,
    aggregate: &str,
) -> u64 {
    let query = if managed {
        "SELECT count(*) FROM brokered_tasks WHERE session_id=?1"
    } else {
        "SELECT count(*) FROM tasks WHERE engagement_id=?1"
    };
    store
        .connection()
        .query_row(query, [aggregate], |row| row.get(0))
        .unwrap()
}

pub fn prepare(root: &Path) -> Value {
    let config = input(root);
    assert_eq!(config["scenario"], "TASK085");
    let prepared = super::prepare(root);
    let home = DolgoraeHome::system().unwrap();
    private(
        &root.join("task085-control-bootstrap.json"),
        &json!({"ready":true}),
    );
    let control_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !root.join("task085-control-ready.json").is_file() {
        assert!(
            std::time::Instant::now() < control_deadline,
            "Control Plane did not become ready"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let carrier =
        CredentialCarrier::open_path(&host_root(root).join("external-controller")).unwrap();
    let managed = config["task085_managed"] == true;
    let primary = if managed {
        let primary_home = root.join("profile-primary");
        fs::create_dir(&primary_home).unwrap();
        fs::set_permissions(&primary_home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::copy(
            root.join("profile/config.toml"),
            primary_home.join("config.toml"),
        )
        .unwrap();
        fs::set_permissions(
            primary_home.join("config.toml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        GlobalProfileStore::new(&home)
            .add(
                "host-primary".into(),
                RuntimeProfile {
                    argv: vec![config["codex"].as_str().unwrap().into()],
                    codex_home: primary_home.to_str().unwrap().into(),
                    environment: BTreeMap::from([
                        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
                        ("LANG".into(), "C".into()),
                        ("LC_ALL".into(), "C".into()),
                    ]),
                    native_subagents: NativeSubagents::Enabled,
                },
            )
            .unwrap();
        let primary_binding = ResolvedGlobalProfile::resolve(&home, "host-primary")
            .unwrap()
            .prepare_qualified(&home)
            .unwrap();
        primary_binding.record_qualified(&home).unwrap();
        private(
            &root.join("task085-primary-binding.json"),
            &json!(primary_binding),
        );
        crate::profile::ensure_qualified_global_server(&primary_binding).unwrap();
        let mut policy: Value = serde_json::from_str(include_str!(
            "../../docs/protocol/examples/workspace-policy-input-v3.valid.json"
        ))
        .unwrap();
        policy["policy_name"] = json!("task085");
        policy["approval_policy"] = json!("fully_delegated");
        policy["roles"][0]["role_ref"] = json!("general");
        policy["roles"][0]["role_source"]["name"] = json!("general");
        let configuration = &mut policy["roles"][0]["agent_configuration"];
        configuration["selected_profile"] = json!("host-primary");
        configuration["model"] = config["executor_model"].clone();
        configuration["default_effort"] = json!("low");
        configuration
            .as_object_mut()
            .unwrap()
            .remove("global_profile_binding_sha256");
        let path = root.join("task085-policy.json");
        private(&path, &policy);
        crate::markdown_policy::MarkdownPolicyRegistry::discover(Some(&root.join("workspace")))
            .unwrap()
            .add_file(&path, "task085")
            .unwrap();
        let mut start: Value = serde_json::from_str(include_str!(
            "../../docs/protocol/examples/workspace-execution-start-defaults.valid.json"
        ))
        .unwrap();
        start["workspace"] = json!(root.join("workspace"));
        start["profile"] = json!("host-primary");
        start["model"] = config["executor_model"].clone();
        start["effort"] = json!("low");
        start["control_mode"] = json!("direct_interactive");
        start["access"] = json!("read-only");
        start["purpose"] = json!({"kind":"interactive","external_label":null});
        start["specialist_policy"] = json!("task085");
        start["idempotency_key"] = json!("task085-root");
        let started = CoreSemanticService
            .start_caller_run(
                serde_json::from_value(start).unwrap(),
                CredentialCarrier::duplicate_fd(carrier.raw_fd()).unwrap(),
            )
            .unwrap();
        let id = started.snapshot.manifest.run_id;
        let directory = root.join("managed-host");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::copy(
            host_root(root).join("external-controller"),
            directory.join("controller"),
        )
        .unwrap();
        fs::set_permissions(
            directory.join("controller"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let journal = directory.join("journal");
        fs::create_dir(&journal).unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
        let mut draft =
            serde_json::from_slice::<Value>(&fs::read(host_root(root).join("draft.json")).unwrap())
                .unwrap();
        draft["owner"] = json!({"kind":"managed","session_ref":id});
        draft["controller_file"] = json!(directory.join("controller"));
        draft["native_host_root"] = json!(journal);
        draft["allowed_scopes"] = json!([{"kind":"orchestrated_session","reference":id}]);
        draft["tools"] = json!([
            "dolgorae_specialists_native",
            "dolgorae_specialist_consumer"
        ]);
        draft["adapter_id"] = json!("dolgorae.managed/1");
        let draft_path = directory.join("draft.json");
        private(&draft_path, &draft);
        let operator = CredentialCarrier::open_path(&root.join("operator")).unwrap();
        for operation in [
            "owner-config-create",
            "namespace-setup",
            "host-state-init",
            "preparation-init",
            "managed-connect",
        ] {
            let mut args: Vec<OsString> = [
                operation.to_owned(),
                "--owner-config".into(),
                directory.join("owner.json").to_str().unwrap().into(),
                "--operator-fd".into(),
                operator.raw_fd().to_string(),
                "--controller-fd".into(),
                carrier.raw_fd().to_string(),
            ]
            .into_iter()
            .map(OsString::from)
            .collect();
            if operation == "owner-config-create" {
                args.extend([
                    OsString::from("--workspace"),
                    root.join("workspace").into_os_string(),
                    OsString::from("--request"),
                    draft_path.as_os_str().to_owned(),
                ]);
            }
            crate::trusted_host::run_admin(&args)
                .unwrap_or_else(|error| panic!("{operation}: {error:?}"));
        }
        let owner = VerifiedOwner::load(&home, &directory.join("owner.json")).unwrap();
        crate::native_consumer::initialize(&owner, crate::native_consumer::Mode::NativeOnly {})
            .unwrap();
        Some(id)
    } else {
        None
    };
    let owner_path = if managed {
        root.join("managed-host/owner.json")
    } else {
        host_root(root).join("owner.json")
    };
    let owner = VerifiedOwner::load(&home, &owner_path).unwrap();
    let scope = service::DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    };
    let aggregate = primary.unwrap_or_else(|| {
        Uuid::parse_str(
            &service::open_external_engagement(&scope, "task085-open", None)
                .unwrap()
                .0
                .engagement_ref,
        )
        .unwrap()
    });
    let kind = if managed {
        "orchestrated_session"
    } else {
        "external_engagement"
    };
    let snapshot = crate::provisioning::retained_snapshot(
        &owner.state_root,
        prepared["candidate"]["provisioning_ref"].as_str().unwrap(),
    )
    .unwrap();
    let (_, configuration) = snapshot.admitted_configuration().unwrap();
    let task = crate::task_request::SpecialistTaskRequest {
        purpose: "research".into(),
        brief: "TASK085_SOURCE".into(),
        contexts: vec![],
        criteria: vec![],
        expected_output: "structured_findings_v1".into(),
    };
    let (accepted, _) =
        crate::successor_capture::capture_facade_task(&configuration, &task, "read_only", None)
            .unwrap();
    let source = json!({"schema":"dolgorae.group-task-input/v1","provisioning_ref":prepared["candidate"]["provisioning_ref"],"assignment":{"task_body":task,"accepted_task_identity":accepted,"deadline_seconds":60}});
    let source_bytes = crate::operation::canonical(&source).unwrap();
    let path = root.join("workspace/task085-source.json");
    fs::write(&path, &source_bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let group_request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"assign_group","scope_kind":kind,"scope_ref":aggregate,"idempotency_key":"task085-group","expected_revision":null,
        "manifest":{"schema":"dolgorae.group-assignment/v1","scope_kind":kind,"scope_ref":aggregate,"entries":[{"entry_key":"source","role_snapshot_digest":accepted.role_snapshot_digest,"agent_configuration_digest":accepted.agent_configuration_digest,"accepted_task_digest":accepted.digest().unwrap()}]},
        "inputs":[{"entry_key":"source","absolute_file_path":path,"content":crate::delegation_identity::BytesRef::from_bytes(&source_bytes)}]});
    let port = SemanticRunSettlementPort { carrier: &carrier };
    let assigned = service::research_group::public_call(
        &home,
        &owner.view,
        &carrier,
        primary,
        kind,
        aggregate,
        "assign_group",
        Some("task085-group"),
        None,
        &group_request,
        &port,
    )
    .unwrap();
    let group = assigned["response"]["group_ref"].clone();
    let waited = service::research_group::public_call(&home,&owner.view,&carrier,primary,kind,aggregate,"await_group",None,None,
        &json!({"schema":"dolgorae.external-specialist-request/v5","operation":"await_group","scope_kind":kind,"scope_ref":aggregate,"group_ref":group,"predicate":{"kind":"all"},"timeout_ms":60_000}),&port).unwrap();
    assert_eq!(waited["response"]["predicate_satisfied"], true);
    let coverage = service::research_group::public_call(&home,&owner.view,&carrier,primary,kind,aggregate,"collect_group",None,None,&json!({"schema":"dolgorae.external-specialist-request/v5","operation":"collect_group","scope_kind":kind,"scope_ref":aggregate,"group_ref":group,"cursor":null,"limit":64}),&port).unwrap()["response"]["coverage"].clone();
    assert_eq!(coverage["sources"][0]["state"], "completed");
    fs::remove_file(path).unwrap();
    let member = coverage["sources"][0]["worker_ref"].clone();
    let merge = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"merge_group_results","scope_kind":kind,"scope_ref":aggregate,"group_ref":group,"group_assignment_digest":coverage["group_assignment_digest"],"sources":coverage["sources"],"algorithm":"merge-exact-findings-v1","idempotency_key":"task085-merge","expected_revision":null});
    let synth = crate::task_request::SpecialistTaskRequest {
        purpose: "synthesis".into(),
        brief: "TASK085_SYNTHESIS: synthesize the immutable supplied findings.".into(),
        contexts: vec![],
        criteria: vec![],
        expected_output: "complete_result".into(),
    };
    let (accepted, _) =
        crate::successor_capture::capture_facade_task(&configuration, &synth, "read_only", None)
            .unwrap();
    let namespace = crate::operation::Namespace::delegation(
        &owner.controller.identity,
        &owner.view.workspace_id,
        Some((kind, &aggregate.to_string())),
        "merge_group_results",
        "task085-merge",
    )
    .unwrap();
    let caller = crate::caller_scope::TrustedCallerScope::from_port(
        &crate::trusted_host::ConfiguredCallerPort(&owner),
    )
    .unwrap();
    let db = crate::engagement::EngagementStore::open_read_only(
        &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
    )
    .unwrap();
    assert!(
        crate::engagement::merge_origin_cutoff(db.connection())
            .unwrap()
            .is_none()
    );
    let result = json!({"dolgorae_version":env!("CARGO_PKG_VERSION"),"managed":managed,"scope_kind":kind,"scope_ref":aggregate,"owner_config":owner_path,"merge":merge,"namespace":crate::delegation_identity::LookupNamespace::from(&namespace),
        "synthesis_assignment":{"task_body":synth,"accepted_task_identity":accepted,"deadline_seconds":60},"member_ref":member,"caller_rounds_before":crate::caller_scope::rounds_used(db.connection(),&caller).unwrap(),
        "root_before":primary.map(|id|root_snapshot(&owner,id)),"source_coverage":coverage,
        "task_count_before":accepted_task_count(&db,managed,&aggregate.to_string()),
        "member_before":root_snapshot(&owner,Uuid::parse_str(member.as_str().unwrap()).unwrap())});
    private(&root.join("task085-prepared.json"), &result);
    result
}

pub fn control_plane(root: &Path) -> Value {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    assert_eq!(input(root)["scenario"], "TASK085");
    let stopped = Arc::new(AtomicBool::new(false));
    let input_stopped = Arc::clone(&stopped);
    std::thread::spawn(move || {
        let mut command = String::new();
        let _ = std::io::stdin().read_line(&mut command);
        input_stopped.store(true, Ordering::Release);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(210);
    while !root.join("task085-control-bootstrap.json").is_file() {
        if stopped.load(Ordering::Acquire) {
            return json!({"started":false,"stopped":true});
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Control Plane bootstrap timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
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
        &root.join("task085-control-ready.json"),
        &json!({"pid":std::process::id(),"ready":true}),
    );
    while !stopped.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    runtime.block_on(control.shutdown(tokio::time::Instant::now() + Duration::from_secs(5)));
    drop(gateway);
    runtime.shutdown_timeout(Duration::from_secs(1));
    json!({"started":true,"stopped":true})
}

pub fn run_managed(root: &Path) -> Value {
    let prepared: Value =
        serde_json::from_slice(&fs::read(root.join("task085-prepared.json")).unwrap()).unwrap();
    assert_eq!(prepared["managed"], true);
    let args: Vec<OsString> = [prepared["scope_ref"].as_str().unwrap().to_owned(),"--workspace".into(),root.join("workspace").to_str().unwrap().into(),"--controller-file".into(),host_root(root).join("external-controller").to_str().unwrap().into(),"--message".into(),"TASK085_MANAGED_PRIMARY: execute only the bounded merge, original-key observation and explicitly requested synthesis; no semantic acceptance.".into(),"--idempotency-key".into(),"task085-primary-turn".into(),"--timeout".into(),"120s".into()].into_iter().map(OsString::from).collect();
    let result = CoreSemanticService
        .execute(&SemanticCommand::Run {
            verb: crate::semantic::RunVerb::Send,
            args,
        })
        .unwrap();
    json!(result)
}

pub fn observe(root: &Path) -> Value {
    let prepared: Value =
        serde_json::from_slice(&fs::read(root.join("task085-prepared.json")).unwrap()).unwrap();
    let home = DolgoraeHome::system().unwrap();
    let owner =
        VerifiedOwner::load(&home, Path::new(prepared["owner_config"].as_str().unwrap())).unwrap();
    let scope = service::DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    };
    let kind = prepared["scope_kind"].as_str().unwrap();
    let reference = prepared["scope_ref"].as_str().unwrap();
    let lookup = service::lookup_operation(
        &scope,
        "merge_group_results",
        Some((kind, reference)),
        "task085-merge",
    )
    .unwrap();
    let synth = service::lookup_operation(
        &scope,
        "assign_synthesis",
        Some((kind, reference)),
        "task085-synthesis",
    )
    .unwrap();
    let db = crate::engagement::EngagementStore::open_read_only(
        &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
    )
    .unwrap();
    let caller = crate::caller_scope::TrustedCallerScope::from_port(
        &crate::trusted_host::ConfiguredCallerPort(&owner),
    )
    .unwrap();
    let mut result = json!({"merge_lookup":lookup,"synthesis_lookup":synth,"rounds":crate::caller_scope::rounds_used(db.connection(),&caller).unwrap(),
        "task_count":accepted_task_count(&db,prepared["managed"]==true,reference)});
    if lookup["state"] == "accepted" {
        let receipt: crate::delegation_identity::OperationReceipt =
            serde_json::from_value(lookup["receipt"].clone()).unwrap();
        let merge =
            Uuid::parse_str(receipt.accepted_subjects.merge_ref.as_deref().unwrap()).unwrap();
        let record = service::research_merge::record(&db, merge).unwrap();
        let invocation = crate::storage_observation::read_content(
            db.connection(),
            "research_merge",
            &merge.to_string(),
            "invocation",
        )
        .unwrap()
        .unwrap();
        result["invocation"] = serde_json::from_slice(&invocation).unwrap();
        result["external_context"] =
            json!(crate::engagement::accepted_native_context(db.connection(), &receipt).unwrap());
        let run = Uuid::parse_str(record["owner_run"].as_str().unwrap()).unwrap();
        let snapshot = crate::snapshot::RunSnapshot::load(&owner.state_root, run, 0).unwrap();
        let carrier = CredentialCarrier::open_path(&owner.configuration.controller_file).unwrap();
        let chunk = crate::artifact::chunk(
            &owner.state_root,
            &snapshot,
            record["artifact_id"].as_str().unwrap(),
            (0, 1_048_576),
            Some(&carrier),
        )
        .unwrap();
        assert!(chunk.eof);
        assert_eq!(
            json!(crate::delegation_identity::BytesRef::from_bytes(
                &chunk.data
            )),
            lookup["result_ref"]
        );
        result["artifact_verified"] = json!(true);
    }
    if prepared["managed"] == true {
        result["root"] = root_snapshot(&owner, Uuid::parse_str(reference).unwrap());
    }
    let carrier = CredentialCarrier::open_path(&owner.configuration.controller_file).unwrap();
    let port = SemanticRunSettlementPort { carrier: &carrier };
    result["source_coverage"] = service::research_group::public_call(&home,&owner.view,&carrier,
        (prepared["managed"] == true).then(||Uuid::parse_str(reference).unwrap()),kind,Uuid::parse_str(reference).unwrap(),"collect_group",None,None,
        &json!({"schema":"dolgorae.external-specialist-request/v5","operation":"collect_group","scope_kind":kind,"scope_ref":reference,"group_ref":prepared["merge"]["group_ref"],"cursor":null,"limit":64}),&port).unwrap()["response"]["coverage"].clone();
    result["member"] = root_snapshot(
        &owner,
        Uuid::parse_str(prepared["member_ref"].as_str().unwrap()).unwrap(),
    );
    result
}

// TASK085-only owner cleanup; these administrative calls are not native-call witnesses.
pub fn cleanup_members(root: &Path) -> Result<Vec<Value>, crate::machine::MachineError> {
    use crate::machine::MachineError;
    let read_error = |stage: &str, error: &dyn std::fmt::Display| {
        MachineError::new(
            "NATIVE_FIXTURE_CLEANUP_UNAVAILABLE",
            "the owned cleanup input could not be verified",
            false,
            json!({"stage":stage,"reason":error.to_string()}),
        )
    };
    let home = DolgoraeHome::system()?;
    let view =
        crate::workspace::WorkspaceService::system()?.discover(Some(&root.join("workspace")))?;
    let state = home.workspace_root(&view.workspace_id);
    // Optional only before that Profile has been prepared. Other I/O and decode
    // failures cannot masquerade as an absent cleanup obligation.
    for name in ["qualified-binding.json", "task085-primary-binding.json"] {
        match fs::read(root.join(name)) {
            Ok(bytes) => {
                let _: crate::global_runtime::GlobalProfileBinding =
                    serde_json::from_slice(&bytes).map_err(|error| read_error(name, &error))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(read_error(name, &error)),
        }
    }
    let entries = match fs::read_dir(state.join("runs")) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(read_error("run_inventory", &error)),
    };
    let store = RunStore::new(SystemWorkspacePlatform, &state);
    let mut manifests = Vec::new();
    for entry in entries.into_iter().flatten() {
        let entry = entry.map_err(|error| read_error("run_inventory", &error))?;
        let id = entry
            .file_name()
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
            .ok_or_else(|| read_error("run_inventory", &"invalid Run entry"))?;
        manifests.push(store.load_manifest(id)?);
    }
    let carrier = CredentialCarrier::open_path(&host_root(root).join("external-controller"))?;
    // Fence an unfinished Primary before releasing children, without attempting
    // Close while a successor member is still registered.
    for manifest in manifests.iter().filter(|manifest| {
        manifest.aggregate_binding.as_ref().is_none_or(|binding| {
            binding.member_kind != crate::run::AggregateMemberKind::Specialist
        })
    }) {
        let projection = store.load_state_projection(manifest.run_id)?;
        if projection.active_turn_id.is_some() {
            CoreSemanticService.mutate_caller_run(RunMutationInput {
                workspace: view.canonical_path.to_path_buf()?,
                expected_workspace_id: view.workspace_id.clone(),
                run_id: manifest.run_id,
                expected_state_revision: None,
                carrier: CredentialCarrier::duplicate_fd(carrier.raw_fd())?,
                operation: RunMutationOperation::Pause { interrupt: true },
            })?;
        }
    }
    let mut releases = Vec::new();
    for manifest in &manifests {
        let Some(aggregate) = &manifest.aggregate_binding else {
            continue;
        };
        if aggregate.member_kind != crate::run::AggregateMemberKind::Specialist {
            continue;
        }
        if aggregate.aggregate_kind != crate::domain::AggregateKind::OrchestratedSession {
            return Err(read_error("member_owner", &"unexpected aggregate owner"));
        }
        let session = aggregate.aggregate_id;
        let controller = crate::controller::load_reconciled_controller_binding(&state, session)?;
        let observer = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&state),
        )?;
        let member = crate::orchestration::successor_member(
            observer.connection(),
            session,
            manifest.run_id,
        )?
        .ok_or_else(|| read_error("member_owner", &"successor membership is absent"))?;
        if crate::engagement::successor_spawn_member(
            observer.connection(),
            &member.spawn_operation_id.to_string(),
        )?
        .as_deref()
            != Some(manifest.run_id.to_string().as_str())
        {
            return Err(read_error("member_owner", &"successor spawn differs"));
        }
        let revision = crate::engagement::scope_membership_revision(
            observer.connection(),
            &view.workspace_id,
            "orchestrated_session",
            &session.to_string(),
        )?;
        let pending = {
            let mut statement = observer.connection().prepare(
                "SELECT t.task_id FROM brokered_tasks t JOIN brokered_task_acceptances a USING(task_id) WHERE session_id=?1 AND target_run_id=?2 AND state IN ('accepted','queued','claimed','dispatching','running')")
                .map_err(|error| read_error("pending_tasks", &error))?;
            statement
                .query_map(
                    rusqlite::params![session.to_string(), manifest.run_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| read_error("pending_tasks", &error))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| read_error("pending_tasks", &error))?
        };
        drop(observer);
        let scope = service::DelegationScope {
            view: &view,
            state_root: &state,
            controller: &controller,
            trusted_invocation: None,
        };
        let port = SemanticRunSettlementPort { carrier: &carrier };
        let context = service::ManagedCallContext {
            session_id: session,
            source_run_id: session,
            source_turn_id: "fixture-cleanup",
            source_tool_call_id: "fixture-cleanup-release",
            idempotency_key: "fixture-cleanup-release",
        };
        for task in pending {
            service::cancel_specialist_task(
                &scope,
                service::SettlementOwner::Managed {
                    session_id: session,
                },
                Some(&context),
                &port,
                manifest.run_id,
                Uuid::parse_str(&task).map_err(|error| read_error("pending_tasks", &error))?,
                revision,
                "owned fixture cleanup",
            )?;
        }
        let released = service::release_specialist(
            &scope,
            &context,
            &port,
            service::ReleaseSpecialist {
                member_ref: manifest.run_id,
                expected_revision: revision,
                allow_interrupt: true,
            },
        )?;
        if released.state != "released" || released.operation.progress != "terminal" {
            return Err(MachineError::new(
                "NATIVE_FIXTURE_CLEANUP_UNSETTLED",
                "the member release remains unsettled",
                false,
                json!({"member_ref":manifest.run_id,"state":released.state,"operation":released.operation}),
            ));
        }
        releases.push(
            json!({"member_ref":manifest.run_id,"state":released.state,"revision":released.revision,"operation":released.operation,"administrative_cleanup":true}),
        );
    }
    Ok(releases)
}
