//! Exact production-route fixture preparation. No fake settlement port is used.
use crate::controller::{CredentialCarrier, OperatorStore, create_controller_credential};
use crate::delegation_service as service;
use crate::domain::{ControllerKind, RunLifecycle};
use crate::global_profile::GlobalProfileStore;
use crate::global_runtime::ResolvedGlobalProfile;
use crate::instructions::{InstructionSourceStore, SelectedInstructionSource};
use crate::paths::DolgoraeHome;
use crate::provisioning::{ProvisioningAdmission, ProvisioningSources};
use crate::run::RunStore;
use crate::semantic::{
    CoreSemanticService, RunMutationInput, RunMutationOperation, SemanticCommand, SemanticResult,
    SemanticRunSettlementPort, SemanticService,
};
use crate::trusted_host::VerifiedOwner;
use crate::workspace::{NativeSubagents, RuntimeProfile, SystemWorkspacePlatform, WorkspaceMode};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

#[path = "native_task085.rs"]
mod task085;
pub use task085::{
    control_plane as task085_control_plane, observe as task085_observe, prepare as task085_prepare,
    run_managed as task085_run_managed,
};

fn private(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn input(root: &Path) -> Value {
    assert_eq!(
        fs::read(root.join("fixture-owned")).unwrap(),
        b"native-production-host/v1\n"
    );
    assert_eq!(
        fs::canonicalize(std::env::var_os("HOME").unwrap()).unwrap(),
        root.join("home")
    );
    serde_json::from_slice(&fs::read(root.join("input.json")).unwrap()).unwrap()
}
fn host_root(root: &Path) -> PathBuf {
    match input(root)["host"].as_str() {
        Some("codex") => root.join("codex"),
        Some("claude") => root.join("claude"),
        _ => panic!("unknown selected host"),
    }
}
fn git(workspace: &Path, args: &[&str]) {
    assert!(
        Command::new("/usr/bin/git")
            .arg("-C")
            .arg(workspace)
            .args([
                "-c",
                "user.name=Native Host Fixture",
                "-c",
                "user.email=fixture@example.invalid"
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}
fn setup(root: &Path, operation: &str, extra: &[&str]) {
    use std::os::fd::AsRawFd as _;
    input(root);
    // CredentialCarrier descriptors are inherited only through the existing
    // setup parser's explicit FD arguments, never copied into model input.
    let operator_file = fs::File::open(root.join("operator")).unwrap();
    let controller_file = fs::File::open(host_root(root).join("external-controller")).unwrap();
    let owner = host_root(root).join("owner.json");
    let mut args = vec![
        operation.to_owned(),
        "--owner-config".into(),
        owner.to_str().unwrap().into(),
        "--operator-fd".into(),
        operator_file.as_raw_fd().to_string(),
        "--controller-fd".into(),
        controller_file.as_raw_fd().to_string(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    // Files opened by Rust are CLOEXEC. Use the setup entrypoint in this process
    // with the same explicit descriptors and normal authorization checks.
    crate::trusted_host::run_admin(
        &args
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>(),
    )
    .unwrap();
}

fn connected(config: &Value) -> bool {
    matches!(
        config["scenario"].as_str(),
        Some(
            "exposurecanary"
                | "Rfreshobservation"
                | "beforeacceptance loss"
                | "afteracceptance/mirror-before loss"
                | "aftermirror replyloss"
                | "physicalhostloss"
                | "exactsessionrecovery"
        )
    )
}

pub fn prepare(root: &Path) -> Value {
    let config = input(root);
    let workspace = root.join("workspace");
    git(&workspace, &["init", "-q"]);
    fs::write(
        workspace.join("file.txt"),
        b"original UTF8 evidence: \"quoted\" ",
    )
    .unwrap();
    let heavy = matches!(
        config["scenario"].as_str(),
        Some("Rfreshobservation" | "backendrestart")
    );
    if heavy {
        let segment = if config["scenario"] == "backendrestart" {
            "\"".repeat(210)
        } else {
            format!("{}\"界", "q".repeat(175))
        };
        let directory = workspace
            .join(&segment)
            .join(&segment)
            .join(&segment)
            .join(&segment);
        fs::create_dir_all(&directory).unwrap();
        fs::remove_file(workspace.join("file.txt")).unwrap();
        for n in 0..10000 {
            fs::write(directory.join(format!("before-{n:05}")), b"").unwrap();
        }
        private(
            &root.join("change-layout.json"),
            &json!({"relative":directory.strip_prefix(&workspace).unwrap().to_str().unwrap(),"files":10000}),
        );
    }
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-q", "-m", "fixture baseline"]);
    fs::write(
        workspace.join(".git/info/exclude"),
        b"/.podway/\n/.dolgorae/\n",
    )
    .unwrap();
    let SemanticResult::Workspace(view) = CoreSemanticService
        .execute(&SemanticCommand::Initialize {
            path: Some(workspace.clone()),
            mode: WorkspaceMode::Git,
        })
        .unwrap()
    else {
        panic!("workspace")
    };
    let home = DolgoraeHome::system().unwrap();
    InstructionSourceStore::new(&home)
        .initialize_templates()
        .unwrap();
    crate::engagement::ensure_delegation_namespace(&home.workspace_root(&view.workspace_id))
        .unwrap();
    create_controller_credential(
        &host_root(root).join("external-controller"),
        if config["task085_managed"] == true {
            ControllerKind::InteractiveClient
        } else {
            ControllerKind::Automation
        },
        "native-host-fixture".into(),
        None,
        (config["task085_managed"] == true).then(|| "task085".to_owned()),
    )
    .unwrap();
    OperatorStore::new(home.operator_root())
        .initialize(&root.join("operator"))
        .unwrap();
    let journal = host_root(root).join(if config["host"] == "codex" {
        "sessions"
    } else {
        "projects"
    });
    fs::create_dir_all(&journal).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    let mut request = json!({"route":config["route"],"executable":config["host_identity"],"owner":{"kind":"external"},"controller_generation":1,
        "controller_file":host_root(root).join("external-controller"),"native_host_root":host_root(root).join(if config["host"]=="codex" {"sessions"}else{"projects"}),
        "allowed_scopes":[{"kind":"workspace","reference":view.workspace_id},{"kind":"external_engagement","reference":null}],
        "tools":["specialist_native","specialist_consumer"],
        "caller_scope":{"binding_ref":"local-specialist","podway_workspace_ref":"native-workspace","podway_session_ref":"native-session","podway_attempt_ref":"native-attempt","delegated_placement_ref":"work","max_rounds":3},
        "adapter_id":"dolgorae.external/1","adapter_version":env!("CARGO_PKG_VERSION")});
    if config["scenario"] == "TASK085" {
        request["caller_scope"]["max_rounds"] = json!(16);
    }
    private(&host_root(root).join("draft.json"), &request);
    if !connected(&config) {
        setup(
            root,
            "owner-config-create",
            &[
                "--workspace",
                workspace.to_str().unwrap(),
                "--request",
                host_root(root).join("draft.json").to_str().unwrap(),
            ],
        );
        setup(root, "namespace-setup", &[]);
        setup(root, "host-state-init", &[]);
        setup(root, "preparation-init", &[]);
        let owner = VerifiedOwner::load(&home, &host_root(root).join("owner.json")).unwrap();
        crate::native_consumer::initialize(&owner, crate::native_consumer::Mode::NativeOnly {})
            .unwrap();
    }
    let initial = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"open_external_engagement","workspace_ref":view.workspace_id,"idempotency_key":"native-open","expected_revision":null});
    let mut result = json!({"workspace_ref":view.workspace_id,"owner_config":host_root(root).join("owner.json"),"initial_request":initial,"requires_result":false,"dolgorae_version":env!("CARGO_PKG_VERSION")});
    if heavy
        || connected(&config)
        || matches!(
            config["scenario"].as_str(),
            Some("exposurecanary" | "reconnect" | "Sloss/duplicate" | "TASK085")
        )
    {
        GlobalProfileStore::new(&home)
            .add(
                "host-fixture".into(),
                RuntimeProfile {
                    argv: vec![config["codex"].as_str().unwrap().into()],
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
        let profile = ResolvedGlobalProfile::resolve(&home, "host-fixture").unwrap();
        let binding = if config["scenario"] == "TASK085" {
            profile.prepare_specialist(&home, false)
        } else {
            profile.prepare_qualified(&home)
        }
        .unwrap();
        assert!(binding.caller_workspace_supported());
        binding.record_qualified(&home).unwrap();
        private(&root.join("qualified-binding.json"), &json!(binding));
        crate::profile::ensure_qualified_global_server(&binding).unwrap();
        let sources = ProvisioningSources {
            instruct: &InstructionSourceStore::new(&home),
            role_name: "general",
            role: SelectedInstructionSource::File(vec!["roles".into(), "general".into()]),
            dependencies: vec![],
            selected_guidance: vec![],
            profile_binding: binding,
            admission: ProvisioningAdmission {
                model: config["executor_model"].as_str().unwrap().into(),
                effort: "low".into(),
                purpose_kind: if heavy { "implementation" } else { "review" }.into(),
                purpose_label: None,
                required_capabilities: vec![],
                execution_lane: "dedicated".into(),
                requested_access: if heavy {
                    "canonical_workspace_write"
                } else {
                    "read_only"
                }
                .into(),
                required_assurance: "best_effort_personal_alpha".into(),
                native_subagent_policy: "disabled".into(),
            },
        };
        let carrier =
            CredentialCarrier::open_path(&host_root(root).join("external-controller")).unwrap();
        let controller = crate::controller::binding_from_carrier(&carrier, 1).unwrap();
        let state = home.workspace_root(&view.workspace_id);
        let scope = service::DelegationScope {
            view: &view,
            state_root: &state,
            controller: &controller,
            trusted_invocation: None,
        };
        let (candidate, _) =
            service::install_provisioning(&scope, &sources, "fixture-install", None).unwrap();
        result["candidate"] = json!(candidate);
        result["requires_result"] = json!(true);
    }
    private(&root.join("prepared.json"), &result);
    result
}

pub fn assign(root: &Path) -> Value {
    let config = input(root);
    let prepared: Value =
        serde_json::from_slice(&fs::read(root.join("prepared.json")).unwrap()).unwrap();
    assert_eq!(prepared["requires_result"], true);
    let home = DolgoraeHome::system().unwrap();
    if connected(&config) {
        let connection: crate::native_consumer::PodwayConnection =
            serde_json::from_slice(&fs::read(root.join("connection.json")).unwrap()).unwrap();
        let mut draft: Value =
            serde_json::from_slice(&fs::read(host_root(root).join("draft.json")).unwrap()).unwrap();
        draft["caller_scope"]["podway_workspace_ref"] = json!(connection.workspace);
        draft["caller_scope"]["podway_session_ref"] = json!(connection.session);
        draft["caller_scope"]["podway_attempt_ref"] = json!(connection.attempt);
        private(&host_root(root).join("draft.json"), &draft);
        setup(
            root,
            "owner-config-create",
            &[
                "--workspace",
                root.join("workspace").to_str().unwrap(),
                "--request",
                host_root(root).join("draft.json").to_str().unwrap(),
            ],
        );
        setup(root, "namespace-setup", &[]);
        setup(root, "host-state-init", &[]);
        setup(root, "preparation-init", &[]);
        let owner = VerifiedOwner::load(&home, &host_root(root).join("owner.json")).unwrap();
        crate::native_consumer::initialize(
            &owner,
            crate::native_consumer::Mode::Delegated {
                connection: Box::new(connection),
            },
        )
        .unwrap();
    }
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
    let (opened, _) =
        service::open_external_engagement(&scope, "fixture-result-open", None).unwrap();
    let engagement = Uuid::parse_str(&opened.engagement_ref).unwrap();
    let hired = service::hire_external_specialist(
        &scope,
        &port,
        service::HireExternalSpecialist {
            engagement_id: engagement,
            provisioning_ref: prepared["candidate"]["provisioning_ref"].as_str().unwrap(),
            role_ref: "general",
            agent_configuration_digest: prepared["candidate"]["agent_configuration_digest"]
                .as_str()
                .unwrap(),
            idempotency_key: "fixture-hire",
            expected_revision: None,
        },
    )
    .unwrap();
    let member = Uuid::parse_str(&hired.member.member_ref).unwrap();
    let heavy = matches!(
        config["scenario"].as_str(),
        Some("Rfreshobservation" | "backendrestart")
    );
    let mut task = crate::task_request::SpecialistTaskRequest {
        purpose: if heavy { "change" } else { "completion" }.into(),
        brief: "native-production-original-evidence".into(),
        contexts: vec![],
        criteria: vec![],
        expected_output: if heavy {
            "complete_result"
        } else {
            "structured_review_v3"
        }
        .into(),
    };
    if root.join("package.json").is_file() {
        let package: Value =
            serde_json::from_slice(&fs::read(root.join("package.json")).unwrap()).unwrap();
        task.contexts.push(crate::task_request::TaskContext {
            id: "delegated-package".into(),
            content: serde_json::to_string(&package["result"]["package"]).unwrap(),
            provenance: "caller".into(),
        });
    }
    if !heavy {
        task.contexts.push(crate::task_request::TaskContext {
            id: "requirement".into(),
            content: "Inspect the original fixture evidence.".into(),
            provenance: "caller".into(),
        });
        task.criteria.push(crate::task_request::TaskCriterion {
            id: "C-1".into(),
            statement: "Report observed fixture evidence.".into(),
            source_context_ids: vec!["requirement".into()],
        });
    }
    let execution = if heavy {
        "canonical_workspace_write"
    } else {
        "read_only"
    };
    let reference = engagement.to_string();
    let namespace = (&crate::operation::Namespace::delegation(
        &owner.controller.identity,
        &owner.view.workspace_id,
        Some(("external_engagement", &reference)),
        "assign_external_specialist_task",
        "fixture-assign",
    )
    .unwrap())
        .into();
    let selected = crate::source_binding::AssignmentSelection {
        namespace,
        member_ref: member,
        task: task.clone(),
        execution_intent: execution.into(),
        deadline_seconds: 60,
        target: None,
    };
    let accepted = crate::source_binding::prepare_assignment(&owner, selected).unwrap();
    let caller = crate::caller_scope::TrustedCallerScope::from_port(
        &crate::trusted_host::ConfiguredCallerPort(&owner),
    )
    .unwrap();
    let (assigned, receipt) = crate::source_binding::assign_external(
        &owner,
        &caller,
        &port,
        service::AssignExternalSpecialistTask {
            engagement_id: engagement,
            member_ref: member,
            task,
            execution_intent: execution,
            target: None,
            deadline_seconds: 60,
            idempotency_key: "fixture-assign",
            expected_revision: None,
        },
        accepted,
    )
    .unwrap();
    let result = json!({"scope_ref":engagement,"member_ref":member,"task_ref":assigned.task_ref,"assignment_receipt":receipt});
    private(&root.join("assigned.json"), &result);
    let task_ref = Uuid::parse_str(&assigned.task_ref).unwrap();
    let (_, timed_out) = service::await_external_specialist_tasks(
        &scope,
        service::AwaitExternalSpecialistTasks {
            engagement_id: engagement,
            member_ref: member,
            tasks: &[task_ref],
            predicate: service::AwaitPredicate::All,
            timeout_seconds: 60,
        },
    )
    .unwrap();
    assert!(
        !timed_out,
        "original producer did not settle within its existing deadline"
    );
    if heavy {
        // The terminal hook publishes the result without the controller's
        // credential. Return its writer through the existing authorized owner
        // operation before asking for a fresh idle export proof.
        crate::semantic::release_external_writer(
            &owner.view,
            &owner.state_root,
            member,
            engagement,
            &carrier,
        )
        .unwrap();
    }
    result
}

pub fn observe(root: &Path) -> Value {
    input(root);
    let home = DolgoraeHome::system().unwrap();
    let owner = VerifiedOwner::load(&home, &host_root(root).join("owner.json")).unwrap();
    let mut result = json!({"namespace_verified":true});
    let store = crate::engagement::EngagementStore::open_read_only(
        &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
    )
    .unwrap();
    let count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM delegation_operations WHERE state='accepted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    result["accepted_operations"] = json!(count);
    if root.join("assigned.json").is_file() {
        let a: Value =
            serde_json::from_slice(&fs::read(root.join("assigned.json")).unwrap()).unwrap();
        let carrier =
            CredentialCarrier::open_path(&host_root(root).join("external-controller")).unwrap();
        let port = SemanticRunSettlementPort { carrier: &carrier };
        let scope = service::DelegationScope {
            view: &owner.view,
            state_root: &owner.state_root,
            controller: &owner.controller,
            trusted_invocation: None,
        };
        let collected = service::collect_external_specialist_task(
            &scope,
            &port,
            service::CollectExternalSpecialistTask {
                engagement_id: Uuid::parse_str(a["scope_ref"].as_str().unwrap()).unwrap(),
                member_ref: Uuid::parse_str(a["member_ref"].as_str().unwrap()).unwrap(),
                task_ref: Uuid::parse_str(a["task_ref"].as_str().unwrap()).unwrap(),
                adapter_id: "dolgorae.external/1",
                adapter_version: env!("CARGO_PKG_VERSION"),
            },
        )
        .unwrap();
        // Close fences the Engagement itself, not its membership counter.
        result["engagement_revision"] = json!(
            service::get_external_engagement(
                &scope,
                Uuid::parse_str(a["scope_ref"].as_str().unwrap()).unwrap()
            )
            .unwrap()
            .revision
        );
        result["task"] = json!(collected.task);
        result["complete_result"] = json!(collected.complete_result);
    }
    result
}

pub fn cleanup(root: &Path) -> Value {
    let config = input(root);
    let managed = config["scenario"] == "TASK085" && config["task085_managed"] == true;
    let releases = if managed {
        match task085::cleanup_members(root) {
            Ok(releases) => releases,
            Err(error) => {
                return json!({"cleanup_complete":false,"owned_root":root,
                "failures":[{"operation":"managed_member_release","code":error.code,"message":error.message,"details":error.details}]});
            }
        }
    } else {
        Vec::new()
    };
    let home = DolgoraeHome::system().unwrap();
    let mut bindings = BTreeMap::new();
    let mut complete = true;
    let mut failures = Vec::new();
    if let Ok(entries) = fs::read_dir(home.root().join("workspaces")) {
        for entry in entries.flatten() {
            let state = entry.path();
            let store = RunStore::new(SystemWorkspacePlatform, &state);
            if let Ok(runs) = fs::read_dir(state.join("runs")) {
                let runs: Vec<_> = runs.flatten().collect();
                for run in runs {
                    let Some(id) = run
                        .file_name()
                        .to_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                    else {
                        continue;
                    };
                    let Ok(manifest) = store.load_manifest(id) else {
                        complete = false;
                        continue;
                    };
                    if let Some(binding) = &manifest.global_profile_binding {
                        bindings.insert(binding.server_key.clone(), binding.clone());
                    }
                    if store
                        .load_state_projection(id)
                        .is_ok_and(|p| p.lifecycle == RunLifecycle::Closed)
                    {
                        continue;
                    }
                    let carrier =
                        CredentialCarrier::open_path(&host_root(root).join("external-controller"))
                            .unwrap();
                    if let Err(error) = CoreSemanticService.mutate_caller_run(RunMutationInput {
                        workspace: manifest.canonical_workspace.to_path_buf().unwrap(),
                        expected_workspace_id: manifest.workspace_id.clone(),
                        run_id: id,
                        expected_state_revision: None,
                        carrier,
                        operation: RunMutationOperation::Close { interrupt: true },
                    }) {
                        complete = false;
                        failures.push(json!({"operation":"close","run_id":id,"code":error.code,"message":error.message,"details":error.details}));
                    }
                }
            }
        }
    }
    for name in ["qualified-binding.json", "task085-primary-binding.json"] {
        if let Ok(bytes) = fs::read(root.join(name)) {
            let binding: crate::global_runtime::GlobalProfileBinding =
                serde_json::from_slice(&bytes).unwrap();
            bindings.insert(binding.server_key.clone(), binding);
        }
    }
    for binding in bindings.values() {
        if let Err(error) = crate::profile::retire_qualified_global_server(
            binding,
            &CredentialCarrier::open_path(&root.join("operator")).unwrap(),
        ) {
            complete = false;
            failures.push(json!({"operation":"retire_profile","code":error.code,"message":error.message,"details":error.details}));
        }
    }
    let mut result = json!({"cleanup_complete":complete,"owned_root":root,"failures":failures});
    if managed {
        result["managed_releases"] = json!(releases);
    }
    result
}
