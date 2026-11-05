//! Exact production-route fixture preparation. No fake settlement port is used.
use crate::controller::{CredentialCarrier, OperatorStore, create_controller_credential};
use crate::delegation_service as service;
use crate::domain::{ControllerKind, RunLifecycle};
use crate::global_profile::GlobalProfileStore;
use crate::global_runtime::ResolvedGlobalProfile;
use crate::instructions::{InstructionSourceStore, SelectedInstructionSource, SelectedNamedSource};
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
#[path = "native_task086.rs"]
mod task086;
pub use task086::{
    control_plane as task086_control_plane, finish as task086_finish, merged as task086_merged,
    unknown as task086_unknown,
};

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
            "consumer-complete"
                | "consumer-rounds"
                | "consumer-release"
                | "consumer-group"
                | "consumer-group-gaps"
                | "consumer-fences"
                | "exposurecanary"
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
        let binding = profile.prepare_specialist(&home, false).unwrap();
        assert!(binding.caller_workspace_supported());
        binding.record_qualified(&home).unwrap();
        private(&root.join("qualified-binding.json"), &json!(binding));
        crate::profile::ensure_qualified_global_server(&binding).unwrap();
        let consumer = config["scenario"]
            .as_str()
            .unwrap()
            .starts_with("consumer-");
        let group = config["scenario"]
            .as_str()
            .unwrap()
            .starts_with("consumer-group");
        let writable = heavy || (consumer && !group);
        if consumer {
            fs::write(home.root().join("roles/general/AGENTS.md"), b"Produce the exact accepted task's original evidence and limitations. Make only its explicitly authorized workspace change. Do not accept or advance any workflow.\n").unwrap();
        }
        let mut sources = ProvisioningSources {
            instruct: &InstructionSourceStore::new(&home),
            role_name: "general",
            role: SelectedInstructionSource::File(vec!["roles".into(), "general".into()]),
            dependencies: if consumer {
                vec![SelectedNamedSource {name:"original-evidence".into(),source:SelectedInstructionSource::Inline("Retain every declared source identity and complete result component; previews do not replace original bytes.".into())}]
            } else {
                vec![]
            },
            selected_guidance: if consumer {
                vec![SelectedNamedSource {name:"producer-boundary".into(),source:SelectedInstructionSource::Inline("A producer completion is a factual delivery, not semantic or user acceptance.".into())}]
            } else {
                vec![]
            },
            profile_binding: binding,
            admission: ProvisioningAdmission {
                model: config["executor_model"].as_str().unwrap().into(),
                effort: "low".into(),
                purpose_kind: if group {
                    "research"
                } else if writable {
                    "implementation"
                } else {
                    "review"
                }
                .into(),
                purpose_label: None,
                required_capabilities: vec![],
                execution_lane: if group {
                    "shared_readonly"
                } else {
                    "dedicated"
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
        if consumer {
            fs::write(home.root().join("roles/general/AGENTS.md"), b"Changed same-name source after original capture; never replace an accepted task's immutable Role.\n").unwrap();
            let (role_changed, role_receipt) =
                service::install_provisioning(&scope, &sources, "fixture-role-drift", None)
                    .unwrap();
            assert_ne!(
                candidate.role_snapshot_digest,
                role_changed.role_snapshot_digest
            );
            sources.admission.effort = "medium".into();
            let (configuration_changed, configuration_receipt) = service::install_provisioning(
                &scope,
                &sources,
                "fixture-configuration-drift",
                None,
            )
            .unwrap();
            assert_eq!(
                role_changed.role_snapshot_digest,
                configuration_changed.role_snapshot_digest
            );
            assert_ne!(
                role_changed.agent_configuration_digest,
                configuration_changed.agent_configuration_digest
            );
            private(
                &root.join("candidate-drift.json"),
                &json!({"original":candidate,"same_name_role":role_changed,"changed_configuration":configuration_changed,"role_receipt":role_receipt,"configuration_receipt":configuration_receipt,"new_execution_tasks":0}),
            );
        }
        result["candidate"] = json!(candidate);
        result["requires_result"] = json!(true);
    }
    private(&root.join("prepared.json"), &result);
    result
}

pub fn assign(root: &Path) -> Value {
    input(root);
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
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assign_in(root, || control.request_stop())
    }));
    runtime.block_on(
        control.shutdown(tokio::time::Instant::now() + std::time::Duration::from_secs(5)),
    );
    drop(gateway);
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn assign_in(root: &Path, mut stop_scheduler: impl FnMut()) -> Value {
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
    crate::worker::native_acceptance_run_diagnostic(
        &owner.state_root,
        member,
        "fixture_assign",
        "begin",
    );
    let original_generation = if config["scenario"] == "consumer-release" {
        Some(
            RunStore::new(SystemWorkspacePlatform, &owner.state_root)
                .load_state_projection(member)
                .unwrap()
                .run_generation,
        )
    } else {
        None
    };
    let heavy = matches!(
        config["scenario"].as_str(),
        Some("Rfreshobservation" | "backendrestart")
    );
    let consumer = config["scenario"]
        .as_str()
        .unwrap()
        .starts_with("consumer-");
    let writable = heavy
        || (consumer
            && !config["scenario"]
                .as_str()
                .unwrap()
                .starts_with("consumer-group"));
    let mut task = crate::task_request::SpecialistTaskRequest {
        purpose: if writable { "change" } else { "completion" }.into(),
        brief: if consumer {
            "TASK086_PRODUCER: preserve original source identities and full result bytes. Make the single declared fixture change when writable; otherwise report only the accepted research input.".into()
        } else {
            "native-production-original-evidence".into()
        },
        contexts: vec![],
        criteria: vec![],
        expected_output: if writable {
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
    if root.join("package-evidence.txt").is_file() {
        task.contexts.push(crate::task_request::TaskContext {
            id: "upstream-source".into(),
            content: fs::read_to_string(root.join("package-evidence.txt")).unwrap(),
            provenance: "caller".into(),
        });
    }
    if !writable {
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
    let execution = if writable {
        "canonical_workspace_write"
    } else {
        "read_only"
    };
    if config["scenario"]
        .as_str()
        .unwrap()
        .starts_with("consumer-group")
    {
        return task086::prepare_group(
            root,
            &owner,
            &port,
            engagement,
            member,
            &prepared["candidate"],
            &task,
        );
    }
    let reference = engagement.to_string();
    let rounds = if config["scenario"] == "consumer-rounds" {
        3
    } else {
        1
    };
    let caller = crate::caller_scope::TrustedCallerScope::from_port(
        &crate::trusted_host::ConfiguredCallerPort(&owner),
    )
    .unwrap();
    let mut last = Value::Null;
    let mut retained_rounds = Vec::new();
    let mut last_deadline = None;
    for round in 1..=rounds + u64::from(rounds == 3) {
        let key = if round == 1 {
            "fixture-assign".to_owned()
        } else {
            format!("fixture-feedback-{round}")
        };
        let mut task = task.clone();
        if round > 1 {
            task.brief.push_str(&format!("\nPrimary feedback round {round}: retain exact source links and report remaining gaps."));
        }
        let namespace = (&crate::operation::Namespace::delegation(
            &owner.controller.identity,
            &owner.view.workspace_id,
            Some(("external_engagement", &reference)),
            "assign_external_specialist_task",
            &key,
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
        let accepted_result = crate::source_binding::assign_external(
            &owner,
            &caller,
            &port,
            service::AssignExternalSpecialistTask {
                engagement_id: engagement,
                member_ref: member,
                task: task.clone(),
                execution_intent: execution,
                target: None,
                deadline_seconds: 60,
                idempotency_key: &key,
                expected_revision: None,
            },
            accepted,
        );
        if round > rounds {
            let error = accepted_result.unwrap_err();
            assert_eq!(error.code, "ROUNDS_EXHAUSTED");
            private(
                &root.join("round-exhaustion.json"),
                &json!({"code":error.code,"rounds":rounds,"member_ref":member}),
            );
            break;
        }
        let (assigned, receipt) = accepted_result.unwrap();
        let replay_selection = crate::source_binding::AssignmentSelection {
            namespace: (&crate::operation::Namespace::delegation(
                &owner.controller.identity,
                &owner.view.workspace_id,
                Some(("external_engagement", &reference)),
                "assign_external_specialist_task",
                &key,
            )
            .unwrap())
                .into(),
            member_ref: member,
            task: task.clone(),
            execution_intent: execution.into(),
            deadline_seconds: 60,
            target: None,
        };
        let replay_preparation =
            crate::source_binding::prepare_assignment(&owner, replay_selection).unwrap();
        let replay = crate::source_binding::assign_external(
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
                idempotency_key: &key,
                expected_revision: None,
            },
            replay_preparation,
        )
        .unwrap();
        assert_eq!(replay.0.task_ref, assigned.task_ref);
        assert_eq!(replay.1, receipt);
        let result = json!({"scope_ref":engagement,"member_ref":member,"task_ref":assigned.task_ref,"assignment_receipt":receipt});
        private(&root.join("assigned.json"), &result);
        let task_ref = Uuid::parse_str(&assigned.task_ref).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let recovery_requested = std::cell::Cell::new(false);
        let observe_recovery = || -> Result<(), crate::machine::MachineError> {
            if !recovery_requested.get() {
                let writer_store = crate::writer::WriterStore::new(
                    &owner.state_root,
                    &owner.view.workspace_id,
                    crate::darwin::DarwinSystem.current_uid(),
                );
                let writer = if writer_store.record_exists() {
                    Some(writer_store.load()?)
                } else {
                    None
                };
                if writer
                    .as_ref()
                    .is_some_and(|w| w.state == crate::writer::WriterAuthorityState::BlockedUnknown)
                {
                    return Err(crate::scheduler::error("native_recovery_writer_unknown"));
                }
                if let Some(pending) = writer
                    .as_ref()
                    .and_then(|w| w.pending_generation_release(member))
                {
                    let generation = RunStore::new(SystemWorkspacePlatform, &owner.state_root)
                        .load_state_projection(member)?
                        .run_generation;
                    assert_eq!(pending.source.run_id, member);
                    let mut generations = vec![generation, pending.source.run_generation];
                    if matches!(
                        pending.phase,
                        crate::writer::GenerationReleasePhase::ReaderStarting
                            | crate::writer::GenerationReleasePhase::ReaderVerified
                    ) {
                        if let Some(successor) =
                            pending.successor.as_ref().filter(|s| s.attempt_started)
                        {
                            generations.push(successor.run_generation);
                        }
                    }
                    generations.sort_unstable();
                    generations.dedup();
                    let expected = generations
                        .iter()
                        .map(|generation| {
                            let owner = crate::worker::current_startup_owner(
                                &owner.view.workspace_id,
                                member,
                                *generation,
                            )
                            .map_err(|_| {
                                crate::scheduler::error("native_recovery_owner_invalid")
                            })?;
                            Ok(crate::jcs::sha256_hex(
                                &crate::operation::canonical(&json!(owner)).map_err(|_| {
                                    crate::scheduler::error("native_recovery_owner_invalid")
                                })?,
                            ))
                        })
                        .collect::<Result<Vec<_>, crate::machine::MachineError>>()?;
                    let verdict = crate::worker::lifecycle_owner_verdict(
                        &owner.state_root,
                        &owner.view.workspace_id,
                        member,
                    );
                    let (kind, matches_expected) = match &verdict {
                        crate::worker::LifecycleOwnerVerdict::Free => ("free", false),
                        crate::worker::LifecycleOwnerVerdict::HeldVerified(actual) => {
                            ("held_verified", expected.contains(actual))
                        }
                        crate::worker::LifecycleOwnerVerdict::HeldUnverifiable => {
                            ("held_unverifiable", false)
                        }
                    };
                    private(
                        &root.join("original-task-recovery-owner.json"),
                        &json!({"site":"original_task_settlement","kind":kind,"matches_expected":matches_expected,
                            "projection_generation":generation,"expected_generations":generations,
                            "source_generation":pending.source.run_generation,"transaction":pending.transaction_id,
                            "purpose":pending.purpose,"phase":pending.phase,"requested":recovery_requested.get(),
                            "remaining_ms":deadline.saturating_duration_since(std::time::Instant::now()).as_millis()}),
                    );
                    request_original_recovery_once(
                        deadline,
                        &recovery_requested,
                        verdict,
                        &expected,
                        || {
                            private(
                                &root.join("original-task-recovery-before.json"),
                                &json!({"scope_ref":engagement,"member_ref":member,"task_ref":task_ref,
                            "observed_transaction":pending.transaction_id,"generation":generation,"intent":"general_original_task_reconcile"}),
                            );
                            let recovered = service::reconcile_external_member_task(
                                &scope, &port, engagement, member, task_ref,
                            );
                            private(
                                &root.join("original-task-recovery-result.json"),
                                &json!({"returned":recovered.is_ok(),"error_code":recovered.as_ref().err().map(|e|
                            if crate::machine::registered_errors().contains_key(&e.code) { e.code.as_str() } else { "unregistered" }),
                            "observed_transaction_is_not_effect_condition":true,
                            "post_projection":RunStore::new(SystemWorkspacePlatform, &owner.state_root).load_state_projection(member).ok().map(|p|json!({"run_generation":p.run_generation,"lifecycle":p.lifecycle})),
                            "post_writer":writer_store.load().map(|w|json!({"state":w.state,"pending_transaction":w.pending_generation_release(member).map(|p|p.transaction_id)})).unwrap_or(json!({"state":"unreadable"}))}),
                            );
                            recovered.map(|_| ())
                        },
                    )?;
                }
            }
            Ok(())
        };
        let collected = wait_for_original_settlement(
            deadline,
            |timeout_seconds| {
                service::await_external_specialist_tasks(
                    &scope,
                    service::AwaitExternalSpecialistTasks {
                        engagement_id: engagement,
                        member_ref: member,
                        tasks: &[task_ref],
                        predicate: service::AwaitPredicate::All,
                        timeout_seconds: timeout_seconds.min(1),
                    },
                )
                .map(|(tasks, timed_out)| {
                    private(&root.join("last-settlement-observation.json"), &json!({
                        "phase":"await_terminal", "remaining_ms":deadline.saturating_duration_since(std::time::Instant::now()).as_millis(),
                        "timed_out":timed_out,"task_states":tasks.iter().map(|task|task.state.as_str()).collect::<Vec<_>>() }));
                    !timed_out
                })
            },
            || {
                let observed = service::RunSettlementPort::member_writer_observation(
                    &port,
                    service::MemberCleanupObservation {
                        view: scope.view,
                        state_root: scope.state_root,
                        owner: service::SettlementOwner::External {
                            engagement_id: engagement,
                        },
                        member_ref: member,
                    },
                );
                private(&root.join("last-settlement-observation.json"), &json!({
                    "phase":"writer_return", "remaining_ms":deadline.saturating_duration_since(std::time::Instant::now()).as_millis(),
                    "writer":observed.as_ref().ok().map(|w|json!({"holds":w.holds,"blocked_unknown":w.blocked_unknown})),
                    "error":observed.as_ref().err().map(|e| if crate::machine::registered_errors().contains_key(&e.code) {e.code.as_str()} else {"unregistered"}) }));
                observed
            },
            || {
                let observed = service::collect_external_specialist_task(
                    &scope,
                    &port,
                    service::CollectExternalSpecialistTask {
                        engagement_id: engagement,
                        member_ref: member,
                        task_ref,
                        adapter_id: "dolgorae.external/1",
                        adapter_version: env!("CARGO_PKG_VERSION"),
                    },
                );
                private(&root.join("last-settlement-observation.json"), &json!({
                    "phase":"collect_disposition", "remaining_ms":deadline.saturating_duration_since(std::time::Instant::now()).as_millis(),
                    "observation":observed.as_ref().ok().map(|c|json!({"task_state":c.task.state,
                        "disposition":c.disposition.state,"cleanup_pending":c.disposition.cleanup_pending,
                        "counts":[c.disposition.running,c.disposition.queued,c.disposition.claimed,c.disposition.dispatching,c.disposition.unresolved],
                        "digest_matches":c.task.result_digest.as_ref()==Some(&c.disposition.observed_after_result_digest)})),
                    "error":observed.as_ref().err().map(|e| if crate::machine::registered_errors().contains_key(&e.code) {e.code.as_str()} else {"unregistered"}) }));
                observed
            },
                    observe_recovery,
        )
        .unwrap();
        let mut result = result;
        result["task_state"] = json!(collected.task.state);
        private(
            &root.join(format!("settlement-{round}.json")),
            &json!({"task_ref":collected.task.task_ref,"task_state":collected.task.state,
                "result_digest":collected.task.result_digest,"disposition":{
                "state":collected.disposition.state,
                "observed_after_result_digest":collected.disposition.observed_after_result_digest,
                "running":collected.disposition.running,"queued":collected.disposition.queued,
                "claimed":collected.disposition.claimed,"dispatching":collected.disposition.dispatching,
                "unresolved":collected.disposition.unresolved,"cleanup_pending":collected.disposition.cleanup_pending}}),
        );
        last_deadline = Some(deadline);
        retained_rounds.push(result.clone());
        last = result;
        if collected.task.state != "completed" {
            break;
        }
    }
    if config["scenario"] == "consumer-release" && last["task_state"] == "completed" {
        let deadline = last_deadline.unwrap();
        let current_generation = RunStore::new(SystemWorkspacePlatform, &owner.state_root)
            .load_state_projection(member)
            .unwrap()
            .run_generation;
        let mut expected = Vec::new();
        for generation in [original_generation.unwrap(), current_generation] {
            let witness =
                crate::worker::current_startup_owner(&owner.view.workspace_id, member, generation)
                    .unwrap();
            expected.push(crate::jcs::sha256_hex(
                &crate::operation::canonical(&serde_json::to_value(witness).unwrap()).unwrap(),
            ));
        }
        stop_scheduler_and_wait_free(deadline, &expected, &mut stop_scheduler, || {
            crate::worker::lifecycle_owner_verdict(
                &owner.state_root,
                &owner.view.workspace_id,
                member,
            )
        })
        .unwrap();
        let revision = service::get_external_engagement(&scope, engagement)
            .unwrap()
            .members
            .into_iter()
            .find(|m| m.member_ref == member.to_string())
            .unwrap()
            .revision;
        let released = service::release_external_specialist(
            &scope,
            &port,
            service::ReleaseExternalSpecialist {
                engagement_id: engagement,
                member_ref: member,
                allow_interrupt: false,
                expected_revision: revision,
            },
        )
        .unwrap();
        private(
            &root.join("settled-release.json"),
            &json!({"member_ref":released.member_ref,"operation":released.operation,"revision":released.revision}),
        );
        assert!(
            std::time::Instant::now() < deadline,
            "explicit release exceeded original settlement deadline"
        );
    }
    private(&root.join("accepted-rounds.json"), &json!(retained_rounds));
    last
}

fn stop_scheduler_and_wait_free(
    deadline: std::time::Instant,
    expected_owners: &[String],
    stop: impl FnOnce(),
    mut observe: impl FnMut() -> crate::worker::LifecycleOwnerVerdict,
) -> Result<(), crate::machine::MachineError> {
    stop();
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(crate::machine::MachineError::new(
                "OPERATION_TIMEOUT",
                "original scheduler lifecycle handoff exceeded its existing budget",
                false,
                json!({"required_action":"retain_original_work","cleanup_complete":false}),
            ));
        }
        let verdict = observe();
        if std::time::Instant::now() >= deadline {
            continue;
        }
        match verdict {
            crate::worker::LifecycleOwnerVerdict::Free => {
                return Ok(());
            }
            crate::worker::LifecycleOwnerVerdict::HeldVerified(owner)
                if expected_owners.contains(&owner) => {}
            _ => {
                return Err(crate::machine::MachineError::new(
                    "RECOVERY_REQUIRED",
                    "original scheduler lifecycle owner is foreign or unverified",
                    false,
                    json!({"required_action":"restore_state","cleanup_complete":false}),
                ));
            }
        }
        std::thread::sleep(
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(std::time::Duration::from_millis(25)),
        );
    }
}

fn request_original_recovery_once(
    deadline: std::time::Instant,
    requested: &std::cell::Cell<bool>,
    verdict: crate::worker::LifecycleOwnerVerdict,
    expected: &[String],
    recover: impl FnOnce() -> Result<(), crate::machine::MachineError>,
) -> Result<(), crate::machine::MachineError> {
    if requested.get() {
        return Ok(());
    }
    match verdict {
        crate::worker::LifecycleOwnerVerdict::Free => {}
        crate::worker::LifecycleOwnerVerdict::HeldVerified(actual)
            if expected.contains(&actual) =>
        {
            return Ok(());
        }
        _ => return Err(crate::scheduler::error("native_recovery_owner_invalid")),
    }
    let expired = || {
        crate::machine::MachineError::new(
            "OPERATION_TIMEOUT",
            "original recovery budget expired",
            false,
            json!({}),
        )
    };
    if std::time::Instant::now() >= deadline {
        return Err(expired());
    }
    requested.set(true);
    recover()?;
    if std::time::Instant::now() >= deadline {
        return Err(expired());
    }
    Ok(())
}

fn wait_for_original_settlement(
    deadline: std::time::Instant,
    mut await_terminal: impl FnMut(u64) -> Result<bool, crate::machine::MachineError>,
    mut observe_writer: impl FnMut() -> Result<
        service::MemberWriterObservation,
        crate::machine::MachineError,
    >,
    mut observe: impl FnMut() -> Result<service::CollectedTask, crate::machine::MachineError>,
    mut observe_recovery: impl FnMut() -> Result<(), crate::machine::MachineError>,
) -> Result<service::CollectedTask, crate::machine::MachineError> {
    let timeout = || {
        crate::machine::MachineError::new(
            "OPERATION_TIMEOUT",
            "original Task has no settled idle disposition within its existing budget",
            false,
            json!({"required_action":"retain_original_work","cleanup_complete":false}),
        )
    };
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(timeout());
        }
        observe_recovery()?;
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or_else(timeout)?;
        if await_terminal(remaining.as_secs())? {
            break;
        }
        std::thread::sleep(
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(std::time::Duration::from_millis(25)),
        );
    }
    let mut last_conflict = None;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(last_conflict.unwrap_or_else(timeout));
        }
        observe_recovery()?;
        if std::time::Instant::now() >= deadline {
            return Err(last_conflict.unwrap_or_else(timeout));
        }
        let writer = observe_writer()?;
        if writer.blocked_unknown {
            return Err(crate::machine::MachineError::new(
                "RECOVERY_REQUIRED",
                "original member writer is blocked-unknown",
                false,
                json!({"required_action":"restore_state","cleanup_complete":false}),
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err(last_conflict.unwrap_or_else(timeout));
        }
        if !writer.holds {
            match observe() {
                Ok(collected) => {
                    let disposition = &collected.disposition;
                    if matches!(
                        collected.task.state.as_str(),
                        "completed" | "failed" | "cancelled" | "expired"
                    ) && disposition.state == "idle"
                        && !disposition.cleanup_pending
                        && [
                            disposition.running,
                            disposition.queued,
                            disposition.claimed,
                            disposition.dispatching,
                            disposition.unresolved,
                        ]
                        .iter()
                        .all(|n| *n == 0)
                        && collected.task.result_digest.as_ref()
                            == Some(&disposition.observed_after_result_digest)
                        && std::time::Instant::now() < deadline
                    {
                        return Ok(collected);
                    }
                }
                Err(error)
                    if error.code == "RUN_STATE_CONFLICT" && error.details["effect"] == "none" =>
                {
                    last_conflict = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        std::thread::sleep(
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(std::time::Duration::from_millis(25)),
        );
    }
}

pub fn observe(root: &Path) -> Value {
    let config = input(root);
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
        if input(root)["scenario"]
            .as_str()
            .unwrap()
            .starts_with("consumer-")
        {
            let caller = crate::caller_scope::TrustedCallerScope::from_port(
                &crate::trusted_host::ConfiguredCallerPort(&owner),
            )
            .unwrap();
            let identity = service::get_external_delegation_identity(
                &scope,
                &caller,
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
            let configuration = service::preparation_configuration(
                &scope,
                service::SettlementOwner::External {
                    engagement_id: Uuid::parse_str(a["scope_ref"].as_str().unwrap()).unwrap(),
                },
                Uuid::parse_str(a["member_ref"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(
                configuration
                    .admitted_configuration
                    .as_ref()
                    .unwrap()
                    .digest()
                    .unwrap(),
                identity.identity.agent_configuration_digest
            );
            assert_eq!(
                configuration
                    .instructions
                    .delegation()
                    .unwrap()
                    .role_snapshot_digest,
                identity.identity.role_snapshot_digest
            );
            let original = service::original_delegation_task(
                &owner.state_root,
                &identity.identity.complete_result,
            )
            .unwrap();
            result["original_task"] = json!({"body_utf8":String::from_utf8(original.task_body).unwrap(),
                "contexts":original.contexts.into_iter().map(|(id,body)|json!({"id":id,"content":crate::delegation_identity::BytesRef::from_bytes(&body),"body_utf8":String::from_utf8(body).unwrap()})).collect::<Vec<_>>()});
            result["accepted_configuration"] = json!(configuration);
            result["delegation_identity"] = json!({"identity":identity.identity});
            if config["scenario"] == "consumer-complete" {
                let snapshot = crate::snapshot::RunSnapshot::load(
                    &owner.state_root,
                    Uuid::parse_str(a["member_ref"].as_str().unwrap()).unwrap(),
                    0,
                )
                .unwrap();
                let ledger = snapshot.observed_ledger(&owner.state_root).unwrap();
                let decisions = ledger
                    .records()
                    .iter()
                    .filter(|record| record.kind() == crate::audit::AuditKind::ApprovalDecided)
                    .map(|record| {
                        let value: Value = serde_json::from_slice(
                            &crate::jcs::canonicalize(record.payload()).unwrap(),
                        )
                        .unwrap();
                        assert_eq!(value["resolution"]["decision"], "decline");
                        assert_eq!(
                            value["resolution"]["policy_cause"],
                            "SPECIALIST_INTERACTION_UNSUPPORTED"
                        );
                        json!({"sequence":record.sequence(),"resolution":value["resolution"]})
                    })
                    .collect::<Vec<_>>();
                assert_eq!(decisions.len(), 2);
                result["policy_declines"] = json!(decisions);
            }
        }
    }
    result
}

pub fn peer_read(root: &Path) -> Value {
    use crate::podway_client::Client as _;
    input(root);
    let connection: crate::native_consumer::PodwayConnection =
        serde_json::from_slice(&fs::read(root.join("peer-connection.json")).unwrap()).unwrap();
    assert!(connection.worktree.starts_with(root));
    let reply = crate::podway_client::PublicCli
        .invoke(
            &connection,
            crate::podway_client::Invocation::Read(crate::podway_client::ReadCommand::Observe),
            &[],
            &mut |_| Ok(()),
        )
        .unwrap();
    assert!(reply.success);
    assert_eq!(
        reply.value["result"]["schema"],
        "podway.observation-result/v5"
    );
    let unsupported: crate::native_consumer::PodwayConnection =
        serde_json::from_slice(&fs::read(root.join("unsupported-peer-connection.json")).unwrap())
            .unwrap();
    assert!(unsupported.worktree.starts_with(root));
    assert_eq!(unsupported.executable, connection.executable);
    assert_eq!(unsupported.executable_sha256, connection.executable_sha256);
    let refused = crate::podway_client::PublicCli
        .invoke(
            &unsupported,
            crate::podway_client::Invocation::Read(crate::podway_client::ReadCommand::Observe),
            &[],
            &mut |_| Ok(()),
        )
        .unwrap();
    assert!(!refused.success);
    assert_eq!(refused.value["code"], "WORKSPACE_CONTRACT_MISMATCH");
    assert_eq!(
        refused.value["details"]["expected_contract_manifest_digest"],
        connection.manifest_digest
    );
    assert_eq!(
        refused.value["details"]["actual_contract_manifest_digest"],
        "sha256:57cb8463f0f24033f9bd0437ed1943e77b21b754a888e654bd847bec13f4b91d"
    );
    json!({"compatible_public_response":reply.value,
        "unsupported_peer":{"supported":false,"effect":"not_called","actual_public_response":refused.value}})
}

fn original_operation_unknown(value: &Value, source: &Value) -> bool {
    value["state"] == "accepted"
        && value["current_outcome"] == "unknown"
        && value["accepted_subjects"]["task_ref"] == source["task_ref"]
        && value["accepted_subjects"]["member_ref"] == source["worker_ref"]
        && source["task_ref"].is_string()
        && source["worker_ref"].is_string()
}

fn group_sources_ready(rows: &[Value], gap: bool, fault_and_direct_observed: bool) -> bool {
    rows.len() == 2
        && (!gap || fault_and_direct_observed)
        && rows.iter().all(|row| {
            row["state"] == "completed"
                || (gap && row["entry_key"] == "source-1" && row["state"] == "unknown")
        })
}

fn fault_owned_worker(
    root: &Path,
    owner: &VerifiedOwner,
    engagement: Uuid,
    source: &Value,
    expected: &crate::worker::WorkerIdentity,
) {
    use sha2::{Digest as _, Sha256};
    let member = Uuid::parse_str(source["worker_ref"].as_str().unwrap()).unwrap();
    let snapshot = crate::snapshot::RunSnapshot::load(&owner.state_root, member, 0).unwrap();
    assert_eq!(
        snapshot
            .manifest
            .aggregate_binding
            .as_ref()
            .unwrap()
            .aggregate_id,
        engagement
    );
    let path =
        crate::worker::runtime_record_path(&crate::worker::runtime_root(&owner.state_root), member)
            .unwrap();
    let record =
        crate::worker::read_runtime_record(&path, crate::darwin::DarwinSystem.current_uid())
            .unwrap();
    assert_eq!(
        &record.identity, expected,
        "held Accepted Worker identity changed"
    );
    let frozen = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let digest = format!("{:x}", Sha256::digest(fs::read(&frozen).unwrap()));
    assert_eq!(record.identity.executable_path, frozen);
    assert_eq!(record.identity.executable_sha256, digest);
    assert_eq!(record.binary_sha256, digest);
    assert_eq!(record.identity.run_id, member);
    assert_eq!(record.identity.workspace_id, owner.view.workspace_id);
    assert_eq!(
        record.identity.run_generation,
        snapshot.projection.run_generation
    );
    assert_eq!(
        crate::worker::classify_worker_identity(&record),
        crate::worker::ProcessIdentityVerdict::Match
    );
    let census = crate::darwin::DarwinSystem
        .all_process_identities()
        .unwrap()
        .into_iter()
        .find(|p| p.pid == record.identity.pid)
        .unwrap();
    assert_eq!(census.uid, record.identity.uid);
    assert_eq!(
        census.birth,
        Some((record.identity.start_tvsec, record.identity.start_tvusec))
    );
    assert_eq!(census.process_group_id, record.identity.process_group_id);
    assert_eq!(census.session_id, record.identity.session_id);
    assert_eq!(
        crate::worker::classify_worker_identity(&record),
        crate::worker::ProcessIdentityVerdict::Match
    );
    crate::darwin::DarwinSystem
        .signal_exact_process(&census, libc::SIGKILL)
        .unwrap();
    private(
        &root.join("owned-worker-fault.json"),
        &json!({"run_id":member,"task_ref":source["task_ref"],"engagement_ref":engagement,"pid":census.pid,"birth":census.birth,"uid":census.uid,"process_group_id":census.process_group_id,"session_id":census.session_id,"executable_sha256":digest,"signal":"SIGKILL","accepted_worker_identity":expected}),
    );
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
    if root.join("owned-worker-fault.json").is_file() {
        let owner = VerifiedOwner::load(&home, &host_root(root).join("owner.json")).unwrap();
        let source: Value =
            serde_json::from_slice(&fs::read(root.join("owned-worker-fault.json")).unwrap())
                .unwrap();
        let carrier =
            CredentialCarrier::open_path(&host_root(root).join("external-controller")).unwrap();
        let port = SemanticRunSettlementPort { carrier: &carrier };
        let scope = service::DelegationScope {
            view: &owner.view,
            state_root: &owner.state_root,
            controller: &owner.controller,
            trusted_invocation: None,
        };
        let original_run = Uuid::parse_str(source["run_id"].as_str().unwrap()).unwrap();
        let original_task = Uuid::parse_str(source["task_ref"].as_str().unwrap()).unwrap();
        if let Some(unknown) =
            crate::worker::native_ack_loss_read(&owner.state_root, "native-claim-unknown.json")
                .unwrap()
        {
            assert_eq!(unknown["run_id"], source["run_id"]);
            assert_eq!(unknown["task_ref"], source["task_ref"]);
            if crate::worker::native_ack_loss_read(
                &owner.state_root,
                "native-recovery-release.json",
            )
            .unwrap()
            .is_none()
            {
                crate::worker::native_ack_loss_write(
                    &owner.state_root,
                    "native-recovery-release.json",
                    &crate::worker::native_ack_loss_read(
                        &owner.state_root,
                        "native-claim-unknown.json",
                    )
                    .unwrap()
                    .unwrap(),
                )
                .unwrap();
            }
            let manifest = RunStore::new(SystemWorkspacePlatform, &owner.state_root)
                .load_manifest(original_run)
                .unwrap();
            let recovered = (|| {
                crate::controller::authorize_controller(
                    original_run,
                    "run.reconcile",
                    &manifest.controller,
                    &carrier,
                )?;
                let projection = RunStore::new(SystemWorkspacePlatform, &owner.state_root)
                    .load_state_projection(original_run)?;
                let lease = crate::worker::RunLifecycleLease::acquire(
                    &owner.state_root,
                    &owner.view.workspace_id,
                    original_run,
                    projection.run_generation.max(1),
                )
                .map_err(|e| e.machine_error(original_run, &owner.state_root))?;
                lease
                    .verify_scope(&owner.state_root, &owner.view.workspace_id, original_run)
                    .map_err(|e| e.machine_error(original_run, &owner.state_root))?;
                crate::semantic::recover_scheduler_assignment(
                    &owner.state_root,
                    crate::scheduler::Actor {
                        owner: crate::assignment_queue::Owner::External(
                            Uuid::parse_str(source["engagement_ref"].as_str().unwrap()).unwrap(),
                        ),
                        run: original_run,
                    },
                )
            })();
            if let Err(error) = recovered {
                return json!({"cleanup_complete":false,"required_action":"recover_original_task","code":error.code,"task_ref":original_task});
            }
        }
        let outcome = service::reconcile_external_member_task(
            &scope,
            &port,
            Uuid::parse_str(source["engagement_ref"].as_str().unwrap()).unwrap(),
            Uuid::parse_str(source["run_id"].as_str().unwrap()).unwrap(),
            Uuid::parse_str(source["task_ref"].as_str().unwrap()).unwrap(),
        );
        let terminal = matches!(outcome, Ok(service::MemberSettlement::Terminal));
        private(
            &root.join("original-task-cleanup-reconciliation.json"),
            &json!({"terminal":terminal,"source":source,"error":outcome.err()}),
        );
        if !terminal {
            return json!({"cleanup_complete":false,"required_action":"recover_original_task","owned_root":root});
        }
        let runtime_path = crate::worker::runtime_record_path(
            &crate::worker::runtime_root(&owner.state_root),
            original_run,
        )
        .unwrap();
        let metadata = fs::symlink_metadata(&runtime_path);
        let locator = match &metadata {
            Ok(_) => "present",
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "not_found",
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                "permission_denied"
            }
            Err(_) => "other_error",
        };
        let projection = RunStore::new(SystemWorkspacePlatform, &owner.state_root)
            .load_state_projection(original_run)
            .unwrap();
        private(
            &root.join("original-run-cleanup-readiness.json"),
            &json!({"run_id":original_run,"lifecycle":projection.lifecycle,
                "thread_present":projection.thread_id.is_some(),
                "generation":projection.run_generation,"runtime_locator":locator}),
        );
        if locator == "not_found"
            && projection.lifecycle == RunLifecycle::Idle
            && projection.thread_id.is_some()
        {
            let recovery = CoreSemanticService.mutate_caller_run(RunMutationInput {
                workspace: owner.view.canonical_path.to_path_buf().unwrap(),
                expected_workspace_id: owner.view.workspace_id.clone(),
                run_id: original_run,
                expected_state_revision: None,
                carrier: CredentialCarrier::open_path(&host_root(root).join("external-controller"))
                    .unwrap(),
                operation: RunMutationOperation::Recover,
            });
            private(
                &root.join("original-run-cleanup-recovery.json"),
                &json!({"run_id":original_run,"returned_ok":recovery.is_ok(),
                    "error_code":recovery.as_ref().err().map(|error|&error.code)}),
            );
            if let Err(error) = recovery {
                return json!({"cleanup_complete":false,"required_action":"recover_original_run",
                    "run_id":original_run,"code":error.code});
            }
        }
    }
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
                    crate::worker::native_acceptance_run_diagnostic(
                        &state,
                        id,
                        "fixture_cleanup",
                        "begin",
                    );
                    let runtime_path = crate::worker::runtime_record_path(
                        &crate::worker::runtime_root(&state),
                        id,
                    )
                    .unwrap();
                    let metadata = || {
                        fs::symlink_metadata(&runtime_path).ok().map(|m| json!({
                        "regular":m.file_type().is_file(),"uid":std::os::unix::fs::MetadataExt::uid(&m),
                        "mode":std::os::unix::fs::MetadataExt::mode(&m),"bytes":m.len()}))
                    };
                    let checked = crate::worker::read_runtime_record(
                        &runtime_path,
                        crate::darwin::DarwinSystem.current_uid(),
                    );
                    private(
                        &root.join(format!("cleanup-runtime-{id}.json")),
                        &json!({
                        "metadata_before":metadata(),
                        "checked":checked.as_ref().ok().map(|r|json!({"generation":r.identity.run_generation,
                            "app_server_epoch":r.app_server_epoch,"control_socket_epoch":r.control_socket_epoch,
                            "dedicated_server_present":r.dedicated_server_identity.is_some(),
                            "startup_complete":r.startup_scope.as_ref().map(|s|s.complete)})),
                        "check_error":checked.as_ref().err().map(|e|match e {
                            crate::worker::WorkerProtocolError::InvalidRuntimeRecord=>"invalid_runtime_record",
                            crate::worker::WorkerProtocolError::Io=>"io",_=>"other"})}),
                    );
                    if let Err(error) = CoreSemanticService.mutate_caller_run(RunMutationInput {
                        workspace: manifest.canonical_workspace.to_path_buf().unwrap(),
                        expected_workspace_id: manifest.workspace_id.clone(),
                        run_id: id,
                        expected_state_revision: None,
                        carrier,
                        operation: RunMutationOperation::Close { interrupt: true },
                    }) {
                        private(
                            &root.join(format!("cleanup-runtime-after-error-{id}.json")),
                            &json!({"metadata_after":metadata()}),
                        );
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
    if !complete {
        return json!({"cleanup_complete":false,"owned_root":root,"failures":failures,
            "profile_retirement":"blocked_by_unsettled_run"});
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

#[cfg(test)]
mod settlement_tests {
    use super::*;

    fn collected(state: &str, pending: bool) -> service::CollectedTask {
        let digest = format!("sha256:{}", "a".repeat(64));
        service::CollectedTask {
            task: service::DelegationTaskObservation {
                task_ref: "original-task".into(),
                member_ref: "original-member".into(),
                accepted_task_digest: format!("sha256:{}", "b".repeat(64)),
                state: state.into(),
                result_digest: Some(digest.clone()),
                complete_result: None,
                content_availability: "available".into(),
                required_action: "none".into(),
            },
            complete_result: None,
            disposition: service::DelegationDispositionView {
                state: if pending { "active" } else { "idle" }.into(),
                observed_after_result_digest: digest,
                membership_revision: 1,
                workload_revision: 2,
                running: 0,
                queued: 0,
                claimed: 0,
                dispatching: 0,
                unresolved: 0,
                cleanup_pending: pending,
                release_receipt: None,
            },
        }
    }

    fn writer(holds: bool) -> service::MemberWriterObservation {
        service::MemberWriterObservation {
            holds,
            blocked_unknown: false,
            fingerprint: "original-writer".into(),
        }
    }

    #[test]
    fn gap_readiness_requires_original_fault_unknown_and_direct_sequence() {
        let mut rows = vec![
            json!({"entry_key":"source-0","state":"completed"}),
            json!({"entry_key":"source-1","state":"unknown"}),
        ];
        assert!(!group_sources_ready(&rows, true, false));
        assert!(group_sources_ready(&rows, true, true));
        for state in ["queued", "claimed", "dispatching", "running", "missing"] {
            rows[1]["state"] = json!(state);
            assert!(!group_sources_ready(&rows, true, false));
            assert!(!group_sources_ready(&rows, true, true));
        }
        rows[1]["state"] = json!("completed");
        assert!(group_sources_ready(&rows, false, false));
        assert!(!group_sources_ready(&rows, true, false));
        assert!(group_sources_ready(&rows, true, true));
        assert!(!group_sources_ready(&rows[..1], false, false));
        let source = json!({"task_ref":"original-task","worker_ref":"original-member"});
        let mut observed = json!({"state":"accepted","current_outcome":"unknown",
            "accepted_subjects":{"task_ref":"original-task","member_ref":"original-member"}});
        assert!(original_operation_unknown(&observed, &source));
        for outcome in ["pending", "running", "completed"] {
            observed["current_outcome"] = json!(outcome);
            assert!(!original_operation_unknown(&observed, &source));
        }
        observed["current_outcome"] = json!("unknown");
        observed["accepted_subjects"]["task_ref"] = json!("foreign");
        assert!(!original_operation_unknown(&observed, &source));
        observed["accepted_subjects"]["task_ref"] = json!("original-task");
        observed["accepted_subjects"]["member_ref"] = json!("foreign");
        assert!(!original_operation_unknown(&observed, &source));
    }

    #[test]
    fn explicit_release_waits_for_exact_scheduler_handoff() {
        use crate::worker::LifecycleOwnerVerdict as V;
        use std::cell::RefCell;
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};
        let events = RefCell::new(Vec::new());
        let expected = vec!["original-owner".to_owned()];
        let mut facts = VecDeque::from([V::HeldVerified(expected[0].clone()), V::Free]);
        stop_scheduler_and_wait_free(
            Instant::now() + Duration::from_secs(1),
            &expected,
            || events.borrow_mut().push("stop"),
            || {
                events.borrow_mut().push("observe");
                facts.pop_front().unwrap()
            },
        )
        .unwrap();
        events.borrow_mut().push("release-once");
        assert_eq!(
            *events.borrow(),
            ["stop", "observe", "observe", "release-once"]
        );
        for fact in [V::HeldUnverifiable, V::HeldVerified("foreign-owner".into())] {
            let result = stop_scheduler_and_wait_free(
                Instant::now() + Duration::from_secs(1),
                &expected,
                || {},
                || fact.clone(),
            );
            assert_eq!(result.unwrap_err().code, "RECOVERY_REQUIRED");
        }
        let result = stop_scheduler_and_wait_free(
            Instant::now() + Duration::from_millis(10),
            &expected,
            || {},
            || V::HeldVerified(expected[0].clone()),
        );
        assert_eq!(result.unwrap_err().code, "OPERATION_TIMEOUT");
        let result = stop_scheduler_and_wait_free(
            Instant::now() + Duration::from_millis(5),
            &expected,
            || {},
            || {
                std::thread::sleep(Duration::from_millis(10));
                V::Free
            },
        );
        assert_eq!(result.unwrap_err().code, "OPERATION_TIMEOUT");
    }

    #[test]
    fn explicit_original_recovery_is_one_request_within_original_budget() {
        use crate::worker::LifecycleOwnerVerdict as V;
        use std::cell::Cell;
        use std::time::{Duration, Instant};
        let requested = Cell::new(false);
        let effects = Cell::new(0);
        let effect = || {
            effects.set(effects.get() + 1);
            Ok(())
        };
        let until = Instant::now() + Duration::from_secs(1);
        request_original_recovery_once(
            until,
            &requested,
            V::HeldVerified("scheduler".into()),
            &["scheduler".into(), "reader".into()],
            effect,
        )
        .unwrap();
        request_original_recovery_once(
            until,
            &requested,
            V::HeldVerified("reader".into()),
            &["scheduler".into(), "reader".into()],
            effect,
        )
        .unwrap();
        assert!(!requested.get());
        assert_eq!(effects.get(), 0);
        for wrong in [V::HeldVerified("foreign".into()), V::HeldUnverifiable] {
            assert!(
                request_original_recovery_once(
                    until,
                    &requested,
                    wrong,
                    &["scheduler".into(), "reader".into()],
                    effect
                )
                .is_err()
            );
        }
        assert_eq!(effects.get(), 0);
        request_original_recovery_once(
            until,
            &requested,
            V::Free,
            &["scheduler".into(), "reader".into()],
            effect,
        )
        .unwrap();
        request_original_recovery_once(
            until,
            &requested,
            V::Free,
            &["scheduler".into(), "reader".into()],
            effect,
        )
        .unwrap();
        assert_eq!(effects.get(), 1);
        requested.set(false);
        assert_eq!(
            request_original_recovery_once(
                Instant::now(),
                &requested,
                V::Free,
                &["scheduler".into(), "reader".into()],
                effect
            )
            .unwrap_err()
            .code,
            "OPERATION_TIMEOUT"
        );
        assert!(!requested.get());
        assert_eq!(effects.get(), 1);
        let busy = request_original_recovery_once(
            until,
            &requested,
            V::Free,
            &["scheduler".into(), "reader".into()],
            || {
                effects.set(effects.get() + 1);
                Err(crate::machine::MachineError::new(
                    "RUN_BUSY",
                    "owner raced admission",
                    true,
                    json!({}),
                ))
            },
        )
        .unwrap_err();
        assert_eq!(busy.code, "RUN_BUSY");
        assert!(requested.get());
        assert_eq!(effects.get(), 2);
        requested.set(false);
        let late = request_original_recovery_once(
            Instant::now() + Duration::from_millis(2),
            &requested,
            V::Free,
            &["scheduler".into(), "reader".into()],
            || {
                std::thread::sleep(Duration::from_millis(5));
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(late.code, "OPERATION_TIMEOUT");
        assert!(requested.get());
        // A successful effect return still supplies no terminal/result proof.
        let mut reads = 0;
        let result = wait_for_original_settlement(
            Instant::now() + Duration::from_millis(5),
            |_| Ok(false),
            || panic!("writer before terminal"),
            || {
                reads += 1;
                Ok(collected("completed", false))
            },
            || Ok(()),
        );
        assert_eq!(result.unwrap_err().code, "OPERATION_TIMEOUT");
        assert_eq!(reads, 0);
    }

    #[test]
    fn terminal_then_writer_return_precedes_complete_collect() {
        use std::cell::{Cell, RefCell};
        use std::time::{Duration, Instant};
        let events = RefCell::new(Vec::new());
        let terminals = Cell::new(0);
        let writers = Cell::new(0);
        let collected = wait_for_original_settlement(
            Instant::now() + Duration::from_secs(1),
            |_| {
                events.borrow_mut().push("await");
                terminals.set(terminals.get() + 1);
                Ok(terminals.get() == 2)
            },
            || {
                assert_eq!(terminals.get(), 2);
                events.borrow_mut().push("writer");
                writers.set(writers.get() + 1);
                let mut fact = writer(writers.get() <= 3);
                fact.fingerprint =
                    ["reserved", "active", "releasing", "returned"][writers.get() - 1].into();
                Ok(fact)
            },
            || {
                assert_eq!(writers.get(), 4);
                events.borrow_mut().push("collect");
                Ok(collected("completed", false))
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(collected.task.task_ref, "original-task");
        assert_eq!(
            *events.borrow(),
            [
                "await", "await", "writer", "writer", "writer", "writer", "collect"
            ]
        );
    }

    #[test]
    fn readiness_errors_never_reach_complete_collect() {
        use std::time::{Duration, Instant};
        for code in ["RECOVERY_REQUIRED", "INTEGRITY_ERROR"] {
            let error =
                crate::machine::MachineError::new(code, "original failure", false, json!({}));
            let actual = wait_for_original_settlement(
                Instant::now() + Duration::from_secs(1),
                |_| Ok(true),
                || Err(error.clone()),
                || panic!("unreadable writer cannot reach collect"),
                || Ok(()),
            )
            .unwrap_err();
            assert_eq!(actual, error);
            let actual = wait_for_original_settlement(
                Instant::now() + Duration::from_secs(1),
                |_| Err(error.clone()),
                || panic!("failed terminal await cannot reach writer"),
                || panic!("failed terminal await cannot reach collect"),
                || Ok(()),
            )
            .unwrap_err();
            assert_eq!(actual, error);
        }
        let actual = wait_for_original_settlement(
            Instant::now() + Duration::from_secs(1),
            |_| Ok(true),
            || {
                Ok(service::MemberWriterObservation {
                    blocked_unknown: true,
                    ..writer(true)
                })
            },
            || panic!("blocked-unknown writer cannot reach collect"),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(actual.code, "RECOVERY_REQUIRED");
        assert_eq!(actual.details["required_action"], "restore_state");
    }

    #[test]
    fn all_phases_share_one_deadline() {
        use std::time::{Duration, Instant};
        let actual = wait_for_original_settlement(
            Instant::now() + Duration::from_millis(10),
            |seconds| {
                assert_eq!(seconds, 0);
                std::thread::sleep(Duration::from_millis(20));
                Ok(true)
            },
            || panic!("terminal wait must not grant a fresh writer budget"),
            || panic!("terminal wait must not grant a fresh collection budget"),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(actual.code, "OPERATION_TIMEOUT");
        let actual = wait_for_original_settlement(
            Instant::now() + Duration::from_millis(10),
            |_| Ok(true),
            || {
                std::thread::sleep(Duration::from_millis(20));
                Ok(writer(false))
            },
            || panic!("writer observation must not grant a fresh collection budget"),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(actual.code, "OPERATION_TIMEOUT");
        let actual = wait_for_original_settlement(
            Instant::now() + Duration::from_millis(10),
            |_| Ok(true),
            || Ok(writer(true)),
            || panic!("held writer cannot reach collect"),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(actual.code, "OPERATION_TIMEOUT");
        assert_eq!(actual.details["cleanup_complete"], false);
        let actual = wait_for_original_settlement(
            Instant::now() + Duration::from_millis(10),
            |_| Ok(false),
            || panic!("a free writer before Task terminal is not readiness"),
            || panic!("nonterminal Task cannot reach collect"),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(actual.code, "OPERATION_TIMEOUT");
    }

    #[test]
    fn writer_wait_observes_original_recovery_after_task_terminal() {
        use std::cell::Cell;
        use std::time::{Duration, Instant};
        let terminal = Cell::new(false);
        let pending = Cell::new(false);
        let requested = Cell::new(false);
        let effects = Cell::new(0);
        let until = Instant::now() + Duration::from_secs(1);
        let actual = wait_for_original_settlement(
            until,
            |_| {
                terminal.set(true);
                pending.set(true);
                Ok(true)
            },
            || Ok(writer(pending.get())),
            || Ok(collected("completed", false)),
            || {
                if terminal.get() && pending.get() {
                    request_original_recovery_once(
                        until,
                        &requested,
                        crate::worker::LifecycleOwnerVerdict::Free,
                        &["scheduler".into()],
                        || {
                            effects.set(effects.get() + 1);
                            pending.set(false);
                            Ok(())
                        },
                    )?;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(actual.task.task_ref, "original-task");
        assert_eq!(actual.disposition.state, "idle");
        assert_eq!(effects.get(), 1);
        assert!(requested.get());
    }

    #[test]
    fn terminal_success_and_failure_wait_for_original_disposition() {
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};
        for outcome in ["completed", "failed", "cancelled", "expired"] {
            let mut observations = VecDeque::from([
                Ok(collected(outcome, true)),
                Err(crate::machine::MachineError::new(
                    "RUN_STATE_CONFLICT",
                    "generation changes",
                    true,
                    json!({"effect":"none"}),
                )),
                Ok(collected(outcome, false)),
            ]);
            let mut reads = 0;
            let result = wait_for_original_settlement(
                Instant::now() + Duration::from_secs(1),
                |_| Ok(true),
                || Ok(writer(false)),
                || {
                    reads += 1;
                    observations.pop_front().expect("no extra Task or mutation")
                },
                || Ok(()),
            )
            .unwrap();
            assert_eq!(reads, 3);
            assert_eq!(result.task.state, outcome);
            assert_eq!(result.task.task_ref, "original-task");
            assert!(!result.disposition.cleanup_pending);
        }
    }

    #[test]
    fn missing_or_foreign_proof_times_out_without_calling_a_mutator() {
        use std::time::{Duration, Instant};
        for variant in 0..3 {
            let mut value = collected("completed", false);
            match variant {
                0 => value.disposition.cleanup_pending = true,
                1 => value.disposition.observed_after_result_digest = "foreign-result".into(),
                _ => value.disposition.dispatching = 1,
            }
            let mut reads = 0;
            let result = wait_for_original_settlement(
                Instant::now() + Duration::from_millis(10),
                |_| Ok(true),
                || Ok(writer(false)),
                || {
                    reads += 1;
                    Ok(value.clone())
                },
                || Ok(()),
            )
            .unwrap_err();
            assert_eq!(reads, 1);
            assert_eq!(result.code, "OPERATION_TIMEOUT");
            assert_eq!(result.details["cleanup_complete"], false);
        }
    }

    #[test]
    fn integrity_and_uncertain_effect_errors_are_not_retried() {
        use std::time::{Duration, Instant};
        for (code, effect) in [
            ("INTEGRITY_ERROR", "none"),
            ("RECOVERY_REQUIRED", "none"),
            ("RUN_STATE_CONFLICT", "unknown"),
        ] {
            let expected = crate::machine::MachineError::new(
                code,
                "preserve original",
                false,
                json!({"effect":effect}),
            );
            let mut reads = 0;
            let result = wait_for_original_settlement(
                Instant::now() + Duration::from_secs(1),
                |_| Ok(true),
                || Ok(writer(false)),
                || {
                    reads += 1;
                    Err(expected.clone())
                },
                || Ok(()),
            )
            .unwrap_err();
            assert_eq!(reads, 1);
            assert_eq!(result, expected);
        }
    }
}
