//! Actual native group execution. The Python peer supplies only loopback model output.
use crate::controller::{CredentialCarrier, OperatorStore, create_controller_credential};
use crate::delegation_service as service;
use crate::domain::ControllerKind;
use crate::engagement::EngagementStore;
use crate::global_profile::GlobalProfileStore;
use crate::global_runtime::{GlobalProfileBinding, ResolvedGlobalProfile};
use crate::instructions::{InstructionSourceStore, SelectedInstructionSource};
use crate::paths::DolgoraeHome;
use crate::provisioning::{ProvisioningAdmission, ProvisioningSources};
use crate::semantic::{
    CoreSemanticService, SemanticCommand, SemanticResult, SemanticRunSettlementPort,
    SemanticService,
};
use crate::workspace::{NativeSubagents, RuntimeProfile, WorkspaceMode};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read as _,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

fn private(path: &Path, value: &Value) {
    bytes(path, &serde_json::to_vec(value).unwrap());
}
fn bytes(path: &Path, value: &[u8]) {
    fs::write(path, value).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn verify_root(root: &Path) {
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-research-group/v1\n"
    );
    assert_eq!(fs::canonicalize(root).unwrap(), root);
    assert_eq!(
        fs::canonicalize(std::env::var_os("HOME").unwrap()).unwrap(),
        root.join("home")
    );
    assert_eq!(
        fs::metadata(root).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

fn cleanup_failure(root: &Path, error: crate::machine::MachineError) -> ! {
    private(&root.join("cleanup-failure.json"), &json!(error));
    panic!("cleanup requires explicit recovery: {}", json!(error));
}

fn cleanup_recovery_required(reason: &str) -> crate::machine::MachineError {
    crate::machine::MachineError::new(
        "RECOVERY_REQUIRED",
        "native group cleanup could not establish terminal original work",
        false,
        json!({"reason":reason,"required_action":"recover_run"}),
    )
}

fn terminal_task(state: &str) -> bool {
    matches!(
        state,
        "completed" | "completed_not_delivered" | "delivered" | "failed" | "cancelled" | "expired"
    )
}

fn exclusive_record(path: &Path, value: &Value) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
    file.sync_all().unwrap();
    fs::File::open(path.parent().unwrap())
        .unwrap()
        .sync_all()
        .unwrap();
}

/// Explicit, one-attempt recovery of the previously recorded fault target.
/// Calling this mode requires a separate authorization for this exact fixture.
pub fn recover_original_task(root: &Path, member: Uuid, task: Uuid) -> Value {
    verify_root(root);
    let fault: Value =
        serde_json::from_slice(&fs::read(root.join("owned-worker-fault.json")).unwrap()).unwrap();
    let prior_bytes = fs::read(root.join("cleanup-reconciliation.json")).unwrap();
    let prior: Value = serde_json::from_slice(&prior_bytes).unwrap();
    assert_eq!(fault["identity_verified"], true);
    assert_eq!(fault["run_id"], json!(member));
    assert_eq!(fault["task_ref"], json!(task));
    assert_eq!(prior["member_ref"], json!(member));
    assert_eq!(prior["task_ref"], json!(task));
    assert_eq!(prior["outcome"], "error");
    let context: Value =
        serde_json::from_slice(&fs::read(root.join("group-cleanup-context.json")).unwrap())
            .unwrap();
    let home = DolgoraeHome::system().unwrap();
    let view = crate::workspace::WorkspaceService::system()
        .unwrap()
        .discover(Some(&root.join("workspace")))
        .unwrap();
    assert_eq!(context["workspace_id"], view.workspace_id);
    let engagement = Uuid::parse_str(context["engagement"].as_str().unwrap()).unwrap();
    let carrier = CredentialCarrier::open_path(&root.join("external-controller")).unwrap();
    let controller = crate::controller::binding_from_carrier(&carrier, 1).unwrap();
    let state = home.workspace_root(&view.workspace_id);
    let scope = service::DelegationScope {
        view: &view,
        state_root: &state,
        controller: &controller,
        trusted_invocation: None,
    };
    let port = SemanticRunSettlementPort { carrier: &carrier };
    let accepted = service::research_group::public_lookup(
        &state,
        &view.workspace_id,
        &carrier,
        "external_engagement",
        engagement,
        "assign_group",
        "native-50",
    )
    .unwrap();
    assert_eq!(accepted["state"], "accepted");
    assert_eq!(
        accepted["accepted_subjects"]["group_ref"],
        context["group_ref"]
    );
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(&state)).unwrap();
    let before = store.external_task(engagement, task).unwrap();
    assert_eq!(before.specialist_run_id, member);
    assert!(matches!(before.state.as_str(), "running" | "dispatching"));
    assert!(before.turn_id.is_some());
    assert_eq!(json!(before.turn_id), prior["before_turn_id"]);
    drop(store);
    let mut record = json!({"schema":"dolgorae.native-group-explicit-history-recovery/v1","member_ref":member,"task_ref":task,"engagement_ref":engagement,"group_ref":context["group_ref"],"original_cleanup_record_sha256":format!("{:x}",Sha256::digest(&prior_bytes)),"before_state":before.state,"before_turn_id":before.turn_id,"attempted":true});
    exclusive_record(
        &root.join("original-task-history-recovery-attempt.json"),
        &record,
    );
    match service::reconcile_external_member_task(&scope, &port, engagement, member, task) {
        Ok(service::MemberSettlement::Terminal) => record["service_result"] = json!("terminal"),
        Ok(service::MemberSettlement::Running) => record["service_result"] = json!("running"),
        Err(error) => {
            record["service_result"] = json!("error");
            record["error"] = json!(error);
        }
    }
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(&state)).unwrap();
    let after = store.external_task(engagement, task).unwrap();
    record["after_state"] = json!(after.state);
    record["after_turn_id"] = json!(after.turn_id);
    record["terminal_verified"] = json!(
        record["service_result"] == "terminal"
            && terminal_task(&after.state)
            && after.specialist_run_id == member
            && after.turn_id == before.turn_id
    );
    exclusive_record(
        &root.join("original-task-history-recovery-result.json"),
        &record,
    );
    // Never rewrite the original failure, unknown coverage or campaign report.
    assert_eq!(
        fs::read(root.join("cleanup-reconciliation.json")).unwrap(),
        prior_bytes
    );
    record
}

/// History-only recovery of the one identity-verified fault target. Its earlier
/// unknown coverage remains immutable, and this never resubmits its task.
fn reconcile_cleanup_fault(
    root: &Path,
    scope: &service::DelegationScope<'_>,
    carrier: &CredentialCarrier,
    context: &Value,
    engagement: Uuid,
) -> Value {
    let fault_path = root.join("owned-worker-fault.json");
    if !fault_path.exists() {
        return Value::Null;
    }
    let fault: Value = serde_json::from_slice(&fs::read(fault_path).unwrap()).unwrap();
    assert_eq!(fault["identity_verified"], true);
    let member = Uuid::parse_str(fault["run_id"].as_str().unwrap()).unwrap();
    let task = Uuid::parse_str(fault["task_ref"].as_str().unwrap()).unwrap();
    let path = root.join("cleanup-reconciliation.json");
    let read_task = || {
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(scope.state_root))
            .unwrap()
            .external_task(engagement, task)
            .unwrap()
    };
    if path.exists() {
        let prior: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(prior["member_ref"], json!(member));
        assert_eq!(prior["task_ref"], json!(task));
        if prior["outcome"] == "error"
            && root
                .join("original-task-history-recovery-result.json")
                .exists()
        {
            let recovery: Value = serde_json::from_slice(
                &fs::read(root.join("original-task-history-recovery-result.json")).unwrap(),
            )
            .unwrap();
            let current = read_task();
            assert_eq!(
                recovery["schema"],
                "dolgorae.native-group-explicit-history-recovery/v1"
            );
            assert_eq!(recovery["member_ref"], json!(member));
            assert_eq!(recovery["task_ref"], json!(task));
            assert_eq!(
                recovery["original_cleanup_record_sha256"],
                format!("{:x}", Sha256::digest(fs::read(&path).unwrap()))
            );
            assert_eq!(recovery["terminal_verified"], true);
            assert_eq!(recovery["after_turn_id"], json!(current.turn_id));
            assert!(terminal_task(&current.state));
            return json!({"prior_failed_reconciliation":prior,"explicit_history_recovery":recovery});
        }
        if !matches!(
            prior["outcome"].as_str(),
            Some("terminal" | "already_terminal")
        ) || !terminal_task(&read_task().state)
        {
            cleanup_failure(
                root,
                cleanup_recovery_required(
                    "prior cleanup reconciliation did not establish a terminal",
                ),
            );
        }
        return prior;
    }
    let before = read_task();
    assert_eq!(before.specialist_run_id, member);
    let home = DolgoraeHome::system().unwrap();
    let port = SemanticRunSettlementPort { carrier };
    let coverage = service::research_group::public_call(
        &home,
        scope.view,
        carrier,
        None,
        "external_engagement",
        engagement,
        "collect_group",
        None,
        None,
        &json!({"schema":"dolgorae.external-specialist-request/v5","operation":"collect_group","scope_kind":"external_engagement","scope_ref":engagement,"group_ref":context["group_ref"],"cursor":null,"limit":64}),
        &port,
    )
    .unwrap_or_else(|error| cleanup_failure(root, error));
    private(
        &root.join("cleanup-before-reconciliation.json"),
        &coverage["response"],
    );
    let mut record = json!({"schema":"dolgorae.native-group-cleanup-reconciliation/v1","member_ref":member,"task_ref":task,"before_state":before.state,"before_turn_id":before.turn_id,"attempted":false,"outcome":"already_terminal"});
    if !terminal_task(&before.state) {
        record["attempted"] = json!(true);
        record["outcome"] = json!("pending");
        private(&path, &record);
        let reconciled =
            service::reconcile_external_member_task(scope, &port, engagement, member, task);
        match reconciled {
            Ok(service::MemberSettlement::Terminal) => record["service_result"] = json!("terminal"),
            Ok(service::MemberSettlement::Running) => {
                let error = cleanup_recovery_required("original native history is not terminal");
                record["outcome"] = json!("running");
                record["error"] = json!(error);
                private(&path, &record);
                cleanup_failure(root, error);
            }
            Err(error) => {
                record["outcome"] = json!("error");
                record["error"] = json!(error);
                private(&path, &record);
                cleanup_failure(root, error);
            }
        }
    }
    let after = read_task();
    record["after_state"] = json!(after.state);
    record["after_turn_id"] = json!(after.turn_id);
    if !terminal_task(&after.state) {
        record["outcome"] = json!("unsettled");
        private(&path, &record);
        cleanup_failure(
            root,
            cleanup_recovery_required("reconciliation did not settle the stored original task"),
        );
    }
    if record["attempted"] == true {
        record["outcome"] = json!("terminal");
    }
    private(&path, &record);
    record
}

/// Repeatable owner-service cleanup, including a failed campaign with retained bytes.
/// No PID from a fixture file is signalled by this function.
pub fn cleanup(root: &Path) -> Value {
    verify_root(root);
    let home = DolgoraeHome::system().unwrap();
    let mut bindings = BTreeMap::<String, GlobalProfileBinding>::new();
    let mut reconciliation = Value::Null;
    if let Ok(raw) = fs::read(root.join("group-profile-binding.json")) {
        let binding: GlobalProfileBinding = serde_json::from_slice(&raw).unwrap();
        bindings.insert(binding.server_key.clone(), binding);
    }
    if root.join("group-cleanup-context.json").exists() {
        let context: Value =
            serde_json::from_slice(&fs::read(root.join("group-cleanup-context.json")).unwrap())
                .unwrap();
        let view = crate::workspace::WorkspaceService::system()
            .unwrap()
            .discover(Some(&root.join("workspace")))
            .unwrap();
        assert_eq!(view.workspace_id, context["workspace_id"]);
        let engagement = Uuid::parse_str(context["engagement"].as_str().unwrap()).unwrap();
        let carrier = CredentialCarrier::open_path(&root.join("external-controller")).unwrap();
        let controller = crate::controller::binding_from_carrier(&carrier, 1).unwrap();
        let state = home.workspace_root(&view.workspace_id);
        let scope = service::DelegationScope {
            view: &view,
            state_root: &state,
            controller: &controller,
            trusted_invocation: None,
        };
        let observed = service::get_external_engagement(&scope, engagement).unwrap();
        for member in &observed.members {
            let id = Uuid::parse_str(&member.member_ref).unwrap();
            if let Some(binding) =
                crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, &state)
                    .load_manifest(id)
                    .unwrap()
                    .global_profile_binding
            {
                bindings.insert(binding.server_key.clone(), binding);
            }
        }
        if observed.state != "closed" {
            if observed.state != "open" {
                cleanup_failure(
                    root,
                    cleanup_recovery_required(
                        "a prior close admission requires explicit release recovery",
                    ),
                );
            }
            reconciliation = reconcile_cleanup_fault(root, &scope, &carrier, &context, engagement);
            let store =
                EngagementStore::open_read_only(&EngagementStore::workspace_database_path(&state))
                    .unwrap();
            for member in &observed.members {
                let pending = crate::engagement::successor_member_quiescence(
                    store.connection(),
                    &view.workspace_id,
                    "external_engagement",
                    &engagement.to_string(),
                    &member.member_ref,
                )
                .unwrap();
                if pending.running
                    + pending.queued
                    + pending.claimed
                    + pending.dispatching
                    + pending.unresolved
                    > 0
                {
                    cleanup_failure(
                        root,
                        cleanup_recovery_required(
                            "nonterminal or unresolved owned tasks remain before close admission",
                        ),
                    );
                }
            }
            let current = service::get_external_engagement(&scope, engagement).unwrap();
            service::close_external_engagement(
                &scope,
                &SemanticRunSettlementPort { carrier: &carrier },
                engagement,
                current.revision,
                true,
            )
            .unwrap();
        }
        assert_eq!(
            service::get_external_engagement(&scope, engagement)
                .unwrap()
                .state,
            "closed"
        );
    }
    if !bindings.is_empty() {
        let operator = CredentialCarrier::open_path(&root.join("operator")).unwrap();
        for binding in bindings.values() {
            crate::profile::retire_qualified_global_server(binding, &operator).unwrap();
            // Retirement returns false when a prior cleanup already stopped it.
            // In both cases prove the recorded processes and socket are absent.
            let path = home
                .root()
                .join("profiles")
                .join(&binding.server_key)
                .join("state.json");
            if let Some(file) = crate::workspace::open_secure_file_if_present(
                &path,
                crate::darwin::DarwinSystem.current_uid(),
            )
            .unwrap()
            {
                let mut raw = Vec::new();
                file.take(131073).read_to_end(&mut raw).unwrap();
                assert!(raw.len() <= 131072);
                let state: crate::profile::ServerState = serde_json::from_slice(&raw).unwrap();
                assert_ne!(state.lifecycle, "ready");
                assert!(
                    crate::darwin::DarwinSystem
                        .process_group_pids(state.pgid)
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    crate::darwin::DarwinSystem
                        .process_group_pids(state.drainer_pgid)
                        .unwrap()
                        .is_empty()
                );
                assert!(!Path::new(&state.socket_path).exists());
            }
        }
    }
    if reconciliation.is_null() && root.join("cleanup-reconciliation.json").exists() {
        reconciliation =
            serde_json::from_slice(&fs::read(root.join("cleanup-reconciliation.json")).unwrap())
                .unwrap();
    }
    let result = json!({"schema":"dolgorae.native-research-group-cleanup/v1", "owner_services_complete":true, "profile_generations":bindings.len(),"reconciliation":reconciliation});
    private(&root.join("cleanup-result.json"), &result);
    result
}

fn queue_observation(state: &Path, engagement: Uuid, sources: &Value) -> Value {
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state)).unwrap();
    // All owning task getters share this pinned read transaction. A deadline
    // transition cannot count two different tasks as executing on one Run.
    let snapshot = store.connection().unchecked_transaction().unwrap();
    let mut queued = 0usize;
    let mut total = BTreeMap::<String, usize>::new();
    let mut executing = BTreeMap::<String, usize>::new();
    for source in sources.as_array().unwrap() {
        let diagnostic = json!({"entry_key":source["entry_key"],"state":source["state"],"safe_failure_code":source["safe_failure_code"],"worker_ref":source["worker_ref"],"task_ref":source["task_ref"]});
        assert!(
            source["worker_ref"].is_string() && source["task_ref"].is_string(),
            "group entry has no accepted task identity: {diagnostic}"
        );
        let member = source["worker_ref"].as_str().unwrap();
        let task = Uuid::parse_str(source["task_ref"].as_str().unwrap()).unwrap();
        let task = store.external_task(engagement, task).unwrap();
        assert_eq!(task.specialist_run_id.to_string(), member);
        *total.entry(member.into()).or_default() += 1;
        queued += usize::from(task.state == "queued");
        if matches!(task.state.as_str(), "claimed" | "dispatching" | "running") {
            *executing.entry(member.into()).or_default() += 1;
        }
    }
    assert!(
        total.values().all(|count| *count <= 4),
        "member's accepted queue exceeds its four task bound"
    );
    assert!(
        executing.values().all(|count| *count <= 1),
        "concurrent tasks on one actual member"
    );
    drop(snapshot);
    json!({"queued":queued,"executing":executing.values().sum::<usize>(),"members":total.len(),"maximum_tasks_per_member":total.values().max(),"maximum_executing_per_member":executing.values().max().copied().unwrap_or(0)})
}

pub fn campaign(root: &Path, codex: &Path, k: u32) -> Value {
    verify_root(root);
    assert!([1, 4].contains(&k));
    let SemanticResult::Workspace(view) = CoreSemanticService
        .execute(&SemanticCommand::Initialize {
            path: Some(root.join("workspace")),
            mode: WorkspaceMode::NonGit,
        })
        .unwrap()
    else {
        panic!("workspace")
    };
    let home = DolgoraeHome::system().unwrap();
    let instruct = InstructionSourceStore::new(&home);
    instruct.initialize_templates().unwrap();
    bytes(
        &home.root().join("pool-configuration.json"),
        &serde_json::to_vec(&crate::pool::PoolConfiguration {
            readonly_server_limit: k,
            ..Default::default()
        })
        .unwrap(),
    );
    create_controller_credential(
        &root.join("external-controller"),
        ControllerKind::Automation,
        "native-research-group".into(),
        None,
        None,
    )
    .unwrap();
    OperatorStore::new(home.operator_root())
        .initialize(&root.join("operator"))
        .unwrap();
    let carrier = CredentialCarrier::open_path(&root.join("external-controller")).unwrap();
    let controller = crate::controller::binding_from_carrier(&carrier, 1).unwrap();
    let profile = "native-research-group";
    GlobalProfileStore::new(&home)
        .add(
            profile.into(),
            RuntimeProfile {
                argv: vec![codex.to_str().unwrap().into()],
                codex_home: root.join("profile").to_str().unwrap().into(),
                environment: BTreeMap::from([
                    ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
                    ("LANG".into(), "C".into()),
                    ("LC_ALL".into(), "C".into()),
                ]),
                native_subagents: NativeSubagents::Enabled,
            },
        )
        .unwrap();
    let binding = ResolvedGlobalProfile::resolve(&home, profile)
        .unwrap()
        .prepare_specialist(&home, false)
        .unwrap();
    binding.record_qualified(&home).unwrap();
    private(
        &root.join("group-profile-binding.json"),
        &serde_json::to_value(&binding).unwrap(),
    );
    let state = home.workspace_root(&view.workspace_id);
    crate::engagement::ensure_delegation_namespace(&state).unwrap();
    let scope = service::DelegationScope {
        view: &view,
        state_root: &state,
        controller: &controller,
        trusted_invocation: None,
    };
    let sources = ProvisioningSources {
        instruct: &instruct,
        role_name: "group-reader",
        role: SelectedInstructionSource::Inline(
            "Read only the accepted task. Return its checked structured findings output.".into(),
        ),
        dependencies: vec![],
        selected_guidance: vec![],
        profile_binding: binding,
        admission: ProvisioningAdmission {
            model: "gpt-5.6-luna".into(),
            effort: "low".into(),
            purpose_kind: "research".into(),
            purpose_label: None,
            required_capabilities: vec![],
            execution_lane: "shared_readonly".into(),
            requested_access: "read_only".into(),
            required_assurance: "best_effort_personal_alpha".into(),
            native_subagent_policy: "disabled".into(),
        },
    };
    let (candidate, _) =
        service::install_provisioning(&scope, &sources, "group-install", None).unwrap();
    let snapshot =
        crate::provisioning::retained_snapshot(&state, &candidate.provisioning_ref).unwrap();
    let (_, configuration) = snapshot.admitted_configuration().unwrap();
    let (opened, _) = service::open_external_engagement(&scope, "group-open", None).unwrap();
    let engagement = Uuid::parse_str(&opened.engagement_ref).unwrap();
    private(
        &root.join("group-cleanup-context.json"),
        &json!({"workspace_id":view.workspace_id,"engagement":engagement}),
    );
    let port = SemanticRunSettlementPort { carrier: &carrier };
    let mut entries = Vec::new();
    let mut inputs = Vec::new();
    let mut input_bytes = 0u64;
    for index in 0..50 {
        let entry = format!("task-{index:02}");
        let task = crate::task_request::SpecialistTaskRequest {
            purpose: "research".into(),
            brief: format!("TASK084_NATIVE_ENTRY_{index:02}"),
            contexts: vec![],
            criteria: vec![crate::task_request::TaskCriterion {
                id: "C1".into(),
                statement: "Return the bounded research output.".into(),
                source_context_ids: vec![],
            }],
            expected_output: "structured_findings_v1".into(),
        };
        let (identity, _) =
            crate::successor_capture::capture_facade_task(&configuration, &task, "read_only", None)
                .unwrap();
        let input = json!({"schema":"dolgorae.group-task-input/v1","provisioning_ref":candidate.provisioning_ref,"assignment":{"task_body":task,"accepted_task_identity":identity,"deadline_seconds":if index==48 {1} else {180}}});
        let raw = crate::operation::canonical(&input).unwrap();
        input_bytes += raw.len() as u64;
        let path = root.join("workspace").join(format!("{entry}.json"));
        bytes(&path, &raw);
        inputs.push(json!({"entry_key":entry,"absolute_file_path":path,"content":crate::delegation_identity::BytesRef::from_bytes(&raw)}));
        entries.push(json!({"entry_key":entry,"role_snapshot_digest":identity.role_snapshot_digest,"agent_configuration_digest":identity.agent_configuration_digest,"accepted_task_digest":identity.digest().unwrap()}));
    }
    let request = json!({"manifest":{"schema":"dolgorae.group-assignment/v1","scope_kind":"external_engagement","scope_ref":engagement,"entries":entries},"inputs":inputs});
    let call = |operation: &str, key: Option<&str>, request: &Value| {
        let mut envelope = request.clone();
        envelope["schema"] = json!("dolgorae.external-specialist-request/v5");
        envelope["scope_kind"] = json!("external_engagement");
        envelope["scope_ref"] = json!(engagement);
        envelope["operation"] = json!(operation);
        if operation == "assign_group" {
            envelope["idempotency_key"] = json!(key);
            envelope["expected_revision"] = Value::Null;
        }
        service::research_group::public_call(
            &home,
            &view,
            &carrier,
            None,
            "external_engagement",
            engagement,
            operation,
            key,
            None,
            &envelope,
            &port,
        )
        .unwrap()
    };
    let assigned = call("assign_group", Some("native-50"), &request);
    private(&root.join("group-assign-response.json"), &assigned);
    let group = assigned["response"]["group_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    private(
        &root.join("group-cleanup-context.json"),
        &json!({"workspace_id":view.workspace_id,"engagement":engagement,"group_ref":group}),
    );
    let collect = || {
        call(
            "collect_group",
            None,
            &json!({"group_ref":group,"cursor":null,"limit":64}),
        )["response"]
            .clone()
    };
    let initial = collect();
    private(&root.join("group-initial-coverage.json"), &initial);
    let initial_queue = queue_observation(&state, engagement, &initial["coverage"]["sources"]);
    assert!(
        initial_queue["queued"].as_u64().unwrap() > 0,
        "No actual busy-member queue observed"
    );
    private(
        &root.join("group-assigned.json"),
        &json!({"group_ref":group,"queue":initial_queue}),
    );
    // These accepted queue rows were committed without a gateway. The same
    // central runtime used by serve must recover their lost wake hints.
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let gateway =
        crate::gateway_socket::GatewaySocket::bind(&home, &root.join("gateway.sock")).unwrap();
    let control = {
        let _entered = async_runtime.enter();
        crate::scheduler::ControlPlaneRuntime::start(&home, gateway.record().server_instance_id)
            .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(240);
    while !root.join("normal-post-observed").exists() {
        assert!(
            Instant::now() < deadline,
            "Normal Turn never reached actual loopback"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let busy_queue = queue_observation(&state, engagement, &initial["coverage"]["sources"]);
    private(&root.join("group-busy-queue.json"), &busy_queue);
    assert!(
        busy_queue["executing"].as_u64().unwrap() > 0 && busy_queue["queued"].as_u64().unwrap() > 0,
        "actual busy member and remaining queued work were not observed: {busy_queue}"
    );
    bytes(&root.join("release-normal"), b"release\n");
    while !root.join("unknown-post-observed").exists() {
        assert!(
            Instant::now() < deadline,
            "Unknown Turn never reached actual loopback"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let coverage = collect();
    let unknown = coverage["coverage"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["entry_key"] == "task-49")
        .unwrap();
    let member = Uuid::parse_str(unknown["worker_ref"].as_str().unwrap()).unwrap();
    let path =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(&state), member).unwrap();
    let record =
        crate::worker::read_runtime_record(&path, crate::darwin::DarwinSystem.current_uid())
            .unwrap();
    let manifest = crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, &state)
        .load_manifest(member)
        .unwrap();
    let frozen = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let digest = format!("{:x}", Sha256::digest(fs::read(&frozen).unwrap()));
    assert_eq!(record.identity.executable_path, frozen);
    assert_eq!(record.identity.executable_sha256, digest);
    assert_eq!(record.binary_sha256, digest);
    assert_eq!(record.identity.run_id, member);
    assert_eq!(record.identity.workspace_id, view.workspace_id);
    assert_eq!(
        record.identity.run_generation,
        crate::snapshot::RunSnapshot::load(&state, member, 0)
            .unwrap()
            .projection
            .run_generation
    );
    assert_eq!(
        manifest.aggregate_binding.as_ref().unwrap().aggregate_id,
        engagement
    );
    assert_eq!(
        crate::worker::classify_worker_identity(&record),
        crate::worker::ProcessIdentityVerdict::Match
    );
    let census = crate::darwin::DarwinSystem
        .all_process_identities()
        .unwrap()
        .into_iter()
        .find(|entry| entry.pid == record.identity.pid)
        .unwrap();
    assert_eq!(census.uid, record.identity.uid);
    assert_eq!(
        census.birth,
        Some((record.identity.start_tvsec, record.identity.start_tvusec))
    );
    assert_eq!(census.process_group_id, record.identity.process_group_id);
    assert_eq!(census.session_id, record.identity.session_id);
    // signal_exact_process immediately rechecks BSD birth/UID/group/session;
    // classify additionally binds the worker to the frozen executable bytes.
    assert_eq!(
        crate::worker::classify_worker_identity(&record),
        crate::worker::ProcessIdentityVerdict::Match
    );
    crate::darwin::DarwinSystem
        .signal_exact_process(&census, libc::SIGKILL)
        .unwrap();
    private(
        &root.join("owned-worker-fault.json"),
        &json!({"run_id":member,"task_ref":unknown["task_ref"],"pid":census.pid,"birth":census.birth,"uid":census.uid,"session_id":census.session_id,"process_group_id":census.process_group_id,"executable_sha256":digest,"identity_verified":true,"signal":"SIGKILL"}),
    );
    bytes(&root.join("release-unknown"), b"release\n");
    let any = call(
        "await_group",
        None,
        &json!({"group_ref":group,"predicate":{"kind":"k","k":48},"timeout_ms":120000}),
    );
    assert_eq!(any["response"]["predicate_satisfied"], true);
    let mut last = collect();
    loop {
        let rows = last["coverage"]["sources"].as_array().unwrap();
        let expected = rows.iter().all(|row| match row["entry_key"].as_str() {
            Some("task-49") => row["state"] == "unknown",
            Some("task-48") => row["state"] == "expired",
            Some("task-47") => row["state"] == "failed",
            _ => row["state"] == "completed",
        });
        if expected {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "50-task full coverage did not establish expected actual outcomes"
        );
        std::thread::sleep(Duration::from_millis(100));
        last = collect();
    }
    let rows = last["coverage"]["sources"].as_array().unwrap();
    assert_eq!(rows.len(), 50);
    assert_eq!(last["coverage"]["complete"], true);
    assert!(last["coverage"]["next_cursor"].is_null());
    let mut counts = BTreeMap::<String, u64>::new();
    let mut members = BTreeSet::new();
    let mut bindings = BTreeMap::new();
    for row in rows {
        if row["entry_key"] == "task-47" {
            let store =
                EngagementStore::open_read_only(&EngagementStore::workspace_database_path(&state))
                    .unwrap();
            let task = Uuid::parse_str(row["task_ref"].as_str().unwrap()).unwrap();
            assert_eq!(
                store
                    .external_task(engagement, task)
                    .unwrap()
                    .safe_error_code
                    .as_deref(),
                Some("FINDINGS_OUTPUT_INVALID")
            );
        }
        *counts
            .entry(row["state"].as_str().unwrap().into())
            .or_default() += 1;
        members.insert(Uuid::parse_str(row["worker_ref"].as_str().unwrap()).unwrap());
    }
    // This fixture's compatible, tiny inputs use the current four-entry
    // placement policy. Thirteen is not a universal group contract limit.
    assert_eq!(members.len(), 13);
    for member in &members {
        let snapshot = crate::snapshot::RunSnapshot::load(&state, *member, 0).unwrap();
        assert_eq!(
            snapshot.manifest.execution_lane,
            crate::domain::ExecutionLane::SharedReadonly
        );
        let binding = snapshot.manifest.global_profile_binding.unwrap();
        bindings.insert(binding.server_key.clone(), binding);
    }
    assert!(!bindings.is_empty() && bindings.len() <= k as usize);
    let all = call(
        "await_group",
        None,
        &json!({"group_ref":group,"predicate":{"kind":"all"},"timeout_ms":0}),
    );
    assert_eq!(all["response"]["predicate_satisfied"], false);
    assert_eq!(all["response"]["wait_timed_out"], true);
    let replay = call("assign_group", Some("native-50"), &request);
    assert_eq!(replay["response"]["group_ref"], group);
    assert_eq!(replay["exact_replay"], true);
    assert_eq!(collect()["coverage"]["sources"], json!(rows));
    private(
        &root.join("native-group-result.json"),
        &json!({"schema":"dolgorae.native-research-group/v1","tasks":50,"configured_pool_k":k,"actual_member_runs":members.len(),"server_generations":bindings.len(),"initial_queue":initial_queue,"busy_queue":busy_queue,"states":counts,"input_bytes":input_bytes,"original_key_replay":true,"unknown_not_replayed":true,"coverage":last,"pool_bounds_checked":true}),
    );
    async_runtime.block_on(control.shutdown(tokio::time::Instant::now() + Duration::from_secs(5)));
    drop(gateway);
    async_runtime.shutdown_timeout(Duration::from_secs(1));
    cleanup(root);
    json!({"schema":"dolgorae.native-research-group/v1","passed":true,"tasks":50,"configured_pool_k":k,"actual_member_runs":members.len(),"server_generations":bindings.len(),"states":counts,"initial_queue":initial_queue,"busy_queue":busy_queue,"input_bytes":input_bytes,"cleanup_verified":true,"real_accounts_used":false})
}
