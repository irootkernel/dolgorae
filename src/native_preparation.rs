//! Public Primary bootstrap and policy-bound native preparation.
use crate::delegation_service::{self as service, DelegationScope};
use crate::machine::MachineError;
use crate::trusted_host::{
    AllowedScope, HostExecutable, HostRoute, OwnerDraft, OwnerKind, PrivateDirectory,
    VerifiedOwner, canonical, refusal,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub(crate) const SCHEMA: &str = "dolgorae.managed-preparation/v1";
pub(crate) const RESPONSE_SCHEMA: &str = "dolgorae.managed-preparation-response/v1";

#[derive(Clone, Debug)]
pub(crate) struct Correlation {
    operation: Option<String>,
}
impl Correlation {
    pub(crate) fn from_request(tool: &str, request: &Value) -> Option<Self> {
        (tool == crate::native_consumer::MANAGED_NATIVE && request["schema"] == SCHEMA).then(|| {
            Self {
                operation: request["operation"]
                    .as_str()
                    .filter(|name| ["prepare_provisioning", "prepare_assignment"].contains(name))
                    .map(str::to_owned),
            }
        })
    }
    pub(crate) fn failure(&self, error: &MachineError) -> Value {
        let mut response = crate::specialist_facade::failure(
            self.operation.as_deref().unwrap_or("native_preparation"),
            error,
            false,
        );
        response["schema"] = json!(RESPONSE_SCHEMA);
        response["operation"] = json!(self.operation);
        response["ok"] = json!(false);
        response
    }
}

pub(crate) fn invalid_request() -> MachineError {
    service::successor_error(
        "INVALID_REQUEST",
        "native_preparation",
        None,
        "none",
        "Rejected",
        "fix_request",
        "invalid private preparation request",
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    pub(crate) struct NativeFixture {
        pub f: crate::trusted_host::tests::Fixture,
        pub manifest: crate::run::RunManifest,
    }
    impl NativeFixture {
        pub(crate) fn new(approval: &str) -> Self {
            Self::with_primary_publication(approval, None)
        }

        pub(crate) fn with_primary_publication(
            approval: &str,
            publication: Option<Result<(), crate::orchestration::AdapterFailure>>,
        ) -> Self {
            use crate::run::{CallerRunStartRequest, CallerStartIntent, CallerStartReservation};

            let mut f = crate::trusted_host::tests::Fixture::with_global_generation(
                HostRoute::Codex,
                publication.is_some(),
            );
            std::fs::remove_file(&f.controller).unwrap();
            crate::controller::create_controller_credential(
                &f.controller,
                crate::domain::ControllerKind::HumanCli,
                "native-root-test".into(),
                None,
                None,
            )
            .unwrap();
            let carrier = crate::controller::CredentialCarrier::open_path(&f.controller).unwrap();
            let controller = crate::controller::binding_from_carrier(&carrier, 1).unwrap();
            let template = crate::semantic::successor_fixture::ManagedMember::new();
            let mut manifest = template.manifest.clone();
            let binding = manifest.global_profile_binding.as_ref().unwrap().clone();
            let workspace = crate::workspace::workspace_id(&f.workspace);
            let state = f.home.workspace_root(&workspace);
            let mut value: Value = serde_json::from_slice(include_bytes!(
                "../docs/protocol/examples/accepted-policy-session-v1-captured.valid.json"
            ))
            .unwrap();
            value["captures"]["home_root"] =
                json!(crate::workspace::LosslessPath::from_path(f.home.root()));
            value["captures"]["workspace_id"] = json!(workspace);
            value["captures"]["canonical_workspace"] =
                json!(crate::workspace::LosslessPath::from_path(&f.workspace));
            value["captures"]["policy"]["approval_policy"] = json!(approval);
            value["captures"]["policy"]["max_active_specialists"] = json!(2);
            value["captures"]["policy"]["roles"][0]["max_active_instances"] = json!(1);
            value["captures"]["policy"]["roles"][0]["agent_configuration"]["selected_profile"] =
                json!(binding.selected_name);
            value["captures"]["configurations"]["reviewer"]["selected_profile"] =
                json!(binding.selected_name);
            value["captures"]["profile_bindings"]["reviewer"] = json!(binding);
            value["captures"]["configurations"]["reviewer"]["global_profile_binding_sha256"] =
                json!(binding.digest().unwrap());
            let mut accepted: crate::orchestration::AcceptedPolicySessionSnapshot =
                serde_json::from_value(value).unwrap();
            manifest.run_id = accepted.root_run_id;
            manifest.workspace_id = workspace;
            manifest.canonical_workspace = crate::workspace::LosslessPath::from_path(&f.workspace);
            manifest.controller = controller;
            manifest.control_mode = crate::domain::ControlMode::DirectInteractive;
            manifest.parent_ref = None;
            manifest.agent_configuration.role_reference = None;
            if publication.is_some() {
                let request = CallerRunStartRequest {
                    schema: "dolgorae-run-start-request/v2".into(),
                    workspace: Some(f.workspace.to_str().unwrap().into()),
                    expected_workspace_id: Some(manifest.workspace_id.clone()),
                    profile: binding.selected_name.clone(),
                    control_mode: manifest.control_mode,
                    access: manifest.requested_access.unwrap(),
                    role: manifest.selected_role.clone().unwrap(),
                    specialist_policy: Some(accepted.captures.policy().policy_name.clone()),
                    model: None,
                    effort: None,
                    purpose: manifest.purpose.clone(),
                    parent_ref: None,
                    required_capabilities: manifest.required_capabilities.clone(),
                    idempotency_key: accepted.idempotency_key.clone(),
                }
                .resolve()
                .unwrap();
                let start = CallerStartReservation::new(CallerStartIntent {
                    run_id: manifest.run_id,
                    created_at: manifest.created_at.clone(),
                    workspace_id: manifest.workspace_id.clone(),
                    canonical_workspace: manifest.canonical_workspace.clone(),
                    workspace_mode: manifest.workspace_mode,
                    start_baseline: manifest.start_baseline.clone(),
                    request,
                    controller: manifest.controller.clone(),
                    global_profile_binding: binding,
                    instructions: manifest.instructions.delegation().unwrap().clone(),
                    admitted_configuration: Some(manifest.agent_configuration.clone()),
                    orchestration: Some(accepted.captures.clone()),
                    readiness: crate::run::CallerProfileReadiness {
                        model: manifest.model.clone(),
                        default_effort: manifest.default_reasoning_effort.clone(),
                        profile_capability_snapshot: manifest.profile_capability_snapshot.clone(),
                    },
                })
                .unwrap();
                accepted.request_sha256 = start.normalized_identity_sha256.clone();
                manifest.start_operation_receipt = Some(Some(
                    crate::run::public_start_operation_receipt(&start.accepted_intent).unwrap(),
                ));
                crate::run::StartReservationStore::new(
                    crate::workspace::SystemWorkspacePlatform,
                    &state,
                )
                .reserve_caller_generation(&start)
                .unwrap();
            }
            let mut sessions = crate::orchestration::OrchestrationStore::open(&state).unwrap();
            let accepted = sessions
                .accept_policy_session(
                    accepted.root_run_id,
                    &accepted.idempotency_key,
                    &accepted.request_sha256,
                    accepted.captures,
                )
                .unwrap();
            let session = sessions.prepare_caller_session(&accepted).unwrap();
            sessions.mark_primary_intent(session.session_id).unwrap();
            sessions
                .finish_primary_publication(session.session_id, publication.unwrap_or(Ok(())))
                .unwrap();
            manifest.aggregate_binding = Some(crate::run::AggregateBinding {
                aggregate_kind: crate::domain::AggregateKind::OrchestratedSession,
                aggregate_id: session.session_id,
                operation_id: session.bootstrap_operation_id,
                member_kind: crate::run::AggregateMemberKind::Primary,
                policy_sha256: Some(session.specialist_policy_sha256),
                role_reference: None,
                role_snapshot_sha256: None,
                agent_configuration_sha256: None,
            });
            crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, &state)
                .publish(&manifest)
                .unwrap();
            crate::instructions::InstructionSourceStore::new(&f.home)
                .initialize_templates()
                .unwrap();
            f.draft.owner = OwnerKind::Managed {
                session_ref: session.session_id,
            };
            Self { f, manifest }
        }
        pub(crate) fn state(&self) -> std::path::PathBuf {
            self.f.home.workspace_root(&self.manifest.workspace_id)
        }
        pub(crate) fn boot(&self) -> Result<(), MachineError> {
            let view = crate::workspace::WorkspaceService::new(
                crate::workspace::SystemWorkspacePlatform,
                crate::workspace::SystemGitRunner,
                self.f.home.root().to_owned(),
            )
            .discover(Some(&self.f.workspace))?;
            bootstrap(
                &self.f.home,
                &view,
                &self.manifest,
                &crate::controller::CredentialCarrier::open_path(&self.f.controller)?,
            )
        }
        pub(crate) fn deliver(&self, request: Value, control: bool) -> Value {
            let mut projection =
                crate::projection::RunStateProjection::starting(self.manifest.run_id);
            projection.run_generation = 1;
            let path = crate::run::run_root(&self.state(), self.manifest.run_id).join("state.json");
            std::fs::write(&path, canonical(&projection).unwrap()).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let call = crate::turn::PrimaryToolCall {
                tool_name: if control {
                    crate::native_consumer::MANAGED_CONTROL
                } else {
                    crate::native_consumer::MANAGED_NATIVE
                }
                .into(),
                source_thread_id: "root-thread".into(),
                request_id: 1,
                run_generation: 1,
                context: crate::primary_tool::PrimaryCallContext {
                    session_id: self.manifest.run_id,
                    source_run_id: self.manifest.run_id,
                    source_turn_id: "root-turn".into(),
                    source_tool_call_id: format!(
                        "{}-{}",
                        request["operation"], request["idempotency_key"]
                    ),
                    idempotency_key: "native-test-call".into(),
                },
                payload: request,
            };
            crate::managed_specialist::dispatch_with_port(
                &self.state(),
                &call,
                self.manifest.run_id,
                1,
                &self.manifest.workspace_id,
                &crate::semantic::successor_fixture::AcceptingPort,
            )
        }
        pub(crate) fn owner(&self) -> VerifiedOwner {
            VerifiedOwner::load(
                &self.f.home,
                &owner_path(&self.state(), self.manifest.run_id),
            )
            .unwrap()
        }
    }

    #[test]
    fn native_owner_bootstrap_replays_and_does_not_attach_to_started_workers() {
        for evidence in ["runtime", "bootstrap", "thread", "projection"] {
            let f = NativeFixture::new("fully_delegated");
            let state = f.state();
            let path = match evidence {
                "runtime" => crate::worker::runtime_record_path(
                    &crate::worker::runtime_root(&state),
                    f.manifest.run_id,
                )
                .unwrap(),
                "bootstrap" => crate::worker::bootstrap_path(
                    &crate::worker::runtime_root(&state),
                    f.manifest.run_id,
                ),
                "thread" => crate::run::run_root(&state, f.manifest.run_id)
                    .join("thread-registrations/existing.json"),
                _ => crate::run::run_root(&state, f.manifest.run_id).join("state.json"),
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = if evidence == "projection" {
                let mut projection =
                    crate::projection::RunStateProjection::starting(f.manifest.run_id);
                projection.thread_id = Some("already-accepted".into());
                canonical(&projection).unwrap()
            } else {
                b"existing worker evidence".to_vec()
            };
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            f.boot().unwrap();
            assert!(!owner_path(&state, f.manifest.run_id).exists());
        }
        let f = NativeFixture::new("fully_delegated");
        f.boot().unwrap();
        let owner = f.owner();
        assert!(owner.configuration.caller_scope.is_none());
        let original = std::fs::read(&owner.path).unwrap();
        let executable = f.f.refreshed_image("0.173.0");
        assert_eq!(
            f.f.refresh(&owner.path, &executable, false)
                .unwrap_err()
                .code,
            "RECOVERY_REQUIRED"
        );
        assert_eq!(std::fs::read(&owner.path).unwrap(), original);
        f.boot().unwrap();
        assert_eq!(std::fs::read(&owner.path).unwrap(), original);
        let (_, loaded) = crate::managed_specialist::load(&f.state(), f.manifest.run_id).unwrap();
        assert_eq!(loaded.configuration_digest, owner.configuration_digest);
        let connection =
            crate::run::run_root(&f.state(), f.manifest.run_id).join("specialist-owner.json");
        std::fs::write(&connection, b"corrupt explicit connection").unwrap();
        std::fs::set_permissions(&connection, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(f.boot().is_err());
        assert!(crate::managed_specialist::load(&f.state(), f.manifest.run_id).is_err());
    }

    #[test]
    fn retained_preparations_accept_only_the_authenticated_v1_lineage() {
        let mut f = crate::trusted_host::tests::Fixture::new(HostRoute::Codex);
        f.complete_legacy();
        let old = f.owner();
        let name = "prepared-refresh.json";
        let request = json!({"selection": "exact"});
        let response = json!({"proof": "retained"});
        let prepared = Prepared {
            owner: old.configuration_digest.clone(),
            request: request.clone(),
            response: response.clone(),
        };
        old.root
            .publish(name, &canonical(&prepared).unwrap())
            .unwrap();
        let path = old.root.path.join(name);
        let old_bytes = std::fs::read(&path).unwrap();
        let executable = f.refreshed_image("0.172.1");
        f.refresh(&f.path(), &executable, false).unwrap();
        let current = f.owner();
        assert_eq!(retained(&current, name, &request).unwrap(), Some(response));
        assert_eq!(std::fs::read(&path).unwrap(), old_bytes);
        let mut foreign = prepared;
        foreign.owner = format!("sha256:{}", "0".repeat(64));
        std::fs::write(&path, canonical(&foreign).unwrap()).unwrap();
        assert!(retained(&current, name, &request).is_err());
    }

    #[test]
    fn native_acceptance_counts_all_non_retired_members_and_preserves_typed_denials() {
        let f = NativeFixture::new("fully_delegated");
        f.boot().unwrap();
        let owner = f.owner();
        assert_eq!(roles(&owner).unwrap(), ["reviewer"]);
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE brokered_members(session_id TEXT,role_ref TEXT,membership_state TEXT)").unwrap();
        let context = service::ManagedCallContext {
            session_id: f.manifest.run_id,
            source_run_id: f.manifest.run_id,
            source_turn_id: "turn",
            source_tool_call_id: "call",
            idempotency_key: "key",
        };
        for state in ["provisioning", "active", "degraded", "retiring", "retired"] {
            connection
                .execute("DELETE FROM brokered_members", [])
                .unwrap();
            connection
                .execute(
                    "INSERT INTO brokered_members VALUES(?1,'reviewer',?2)",
                    rusqlite::params![f.manifest.run_id.to_string(), state],
                )
                .unwrap();
            let result = admit_spawn(&connection, &scope(&owner), &context, "reviewer", &owner);
            if state == "retired" {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, "SPECIALIST_POLICY_DENIED");
                assert_eq!(error.details["assignment_refusal"], true);
            }
        }
        connection
            .execute("DELETE FROM brokered_members", [])
            .unwrap();
        for _ in 0..2 {
            connection
                .execute(
                    "INSERT INTO brokered_members VALUES(?1,'another-role','provisioning')",
                    [f.manifest.run_id.to_string()],
                )
                .unwrap();
        }
        assert_eq!(
            admit_spawn(&connection, &scope(&owner), &context, "reviewer", &owner)
                .unwrap_err()
                .code,
            "SPECIALIST_POLICY_DENIED"
        );
        let denied = guard(&owner, &json!({"operation":"spawn_specialist","role_ref":"forbidden","idempotency_key":"denied"})).unwrap_err();
        let response = crate::specialist_facade::failure("spawn_specialist", &denied, false);
        assert_eq!(response["error"]["code"], "SPECIALIST_POLICY_DENIED");
        assert_eq!(response["error"]["required_action"], "fix_request");
        assert_eq!(response["error"]["classification"], "Rejected");
        assert_eq!(response["error"]["effect"], "none");
    }

    #[test]
    fn native_owner_authentication_pins_each_accepted_runtime_and_layout_field() {
        let f = NativeFixture::new("fully_delegated");
        f.boot().unwrap();
        let owner = f.owner();
        let authenticate = |draft: &crate::trusted_host::OwnerDraft| {
            crate::trusted_host::authenticate_owner(
                &f.state(),
                draft,
                &owner.controller,
                &f.manifest.workspace_id,
            )
        };
        authenticate(&owner.configuration).unwrap();
        let controller_copy = owner.root.path.join("controller-copy");
        std::fs::write(
            &controller_copy,
            std::fs::read(&owner.configuration.controller_file).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&controller_copy, std::fs::Permissions::from_mode(0o600)).unwrap();
        let native_copy = owner.root.path.join("native-copy");
        std::fs::create_dir(&native_copy).unwrap();
        std::fs::set_permissions(&native_copy, std::fs::Permissions::from_mode(0o700)).unwrap();
        for field in [
            "path",
            "version",
            "sha256",
            "controller",
            "native",
            "tools",
            "order",
            "route",
        ] {
            let mut draft = owner.configuration.clone();
            match field {
                "path" => draft.executable.path = owner.root.path.join("other-codex"),
                "version" => draft.executable.version = "foreign".into(),
                "sha256" => draft.executable.sha256 = "0".repeat(64),
                "controller" => draft.controller_file = controller_copy.clone(),
                "native" => draft.native_host_root = native_copy.clone(),
                "tools" => draft.tools[0] = "foreign".into(),
                "order" => draft.tools.reverse(),
                "route" => draft.route = HostRoute::Claude,
                _ => unreachable!(),
            }
            // Call the authentication boundary directly: a stale owner digest or
            // an invalid credential must not hide a missing field comparison.
            assert!(authenticate(&draft).is_err(), "native owner field {field}");
        }
        authenticate(&owner.configuration).unwrap();
        owner.revalidate().unwrap();
    }

    #[test]
    fn native_dispatch_status_preparation_replay_and_corruption_are_checked() {
        let f = NativeFixture::new("fully_delegated");
        f.boot().unwrap();
        let owner = f.owner();
        let status = f.deliver(json!({"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"context"}}), true);
        assert_eq!(status["ok"], true, "{status}");
        assert_eq!(
            status["data"]["scope"]["scope_ref"],
            f.manifest.run_id.to_string()
        );
        assert_eq!(status["data"]["allowed_role_refs"], json!(["reviewer"]));
        let request = json!({"schema":SCHEMA,"operation":"prepare_provisioning","role_ref":"reviewer","idempotency_key":"original"});
        let first = f.deliver(request.clone(), false);
        assert_eq!(first["ok"], true, "{first}");
        assert_eq!(f.deliver(request.clone(), false), first);
        let mut changed = request.clone();
        changed["role_ref"] = json!("forbidden");
        let conflict = f.deliver(changed, false);
        assert_eq!(
            conflict["error"]["code"], "IDEMPOTENCY_CONFLICT",
            "{conflict}"
        );
        let install = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"install_provisioning","sources":first["sources"],"idempotency_key":"original","expected_revision":null});
        let installed = f.deliver(install.clone(), false);
        assert_eq!(installed["ok"], true, "{installed}");
        let repeated = f.deliver(request.clone(), false);
        assert_eq!(repeated["sources"], first["sources"]);
        assert_eq!(repeated["lookup"]["state"], "accepted");
        let candidate = &installed["facts"]["candidate"];
        let spawn = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"spawn_specialist",
            "scope_kind":"orchestrated_session","scope_ref":f.manifest.run_id,"role_ref":"reviewer",
            "provisioning_ref":candidate["provisioning_ref"],"agent_configuration_digest":candidate["agent_configuration_digest"],
            "idempotency_key":"spawn-one","expected_revision":null});
        let spawned = f.deliver(spawn.clone(), false);
        assert_eq!(spawned["ok"], true, "{spawned}");
        // The accepting port creates no process or Run record. Publish the
        // member's genuine retained configuration for assignment authentication.
        let member =
            Uuid::parse_str(spawned["facts"]["member"]["member_ref"].as_str().unwrap()).unwrap();
        let broker = crate::orchestration::OrchestrationStore::open(&f.state()).unwrap();
        let facts = broker
            .successor_member(f.manifest.run_id, member)
            .unwrap()
            .unwrap();
        let retained = crate::provisioning::retained_snapshot(
            &f.state(),
            candidate["provisioning_ref"].as_str().unwrap(),
        )
        .unwrap();
        let manifest = crate::delegation_service::tests::member_manifest(
            &owner.view,
            &broker.member_credential(member).unwrap().binding,
            member,
            f.manifest.run_id,
            facts.spawn_operation_id,
            &retained,
        );
        crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, f.state())
            .publish(&manifest)
            .unwrap();
        drop(broker);
        let task_body = json!({"purpose":"research","brief":"Inspect the bounded input.",
            "contexts":[],"criteria":[],"expected_output":"text"});
        let prepare_task = json!({"schema":SCHEMA,"operation":"prepare_assignment",
            "member_ref":spawned["facts"]["member"]["member_ref"],"task_body":task_body,
            "deadline_seconds":120,"idempotency_key":"task-original"});
        let prepared_task = f.deliver(prepare_task, false);
        assert_eq!(prepared_task["ok"], true, "{prepared_task}");
        let assignment = json!({"schema":"dolgorae.external-specialist-request/v5",
            "operation":"assign_specialist_task","scope_kind":"orchestrated_session",
            "scope_ref":f.manifest.run_id,"member_ref":spawned["facts"]["member"]["member_ref"],
            "assignment":{"task_body":task_body,"accepted_task_identity":prepared_task["accepted_task_identity"],
                "deadline_seconds":120},"idempotency_key":"task-original","expected_revision":null});
        let assigned = f.deliver(assignment.clone(), false);
        assert_eq!(assigned["ok"], true, "{assigned}");
        assert!(assigned["facts"]["receipt"].is_object(), "{assigned}");
        assert_eq!(
            assigned["facts"]["receipt"]["accepted_subjects"]["task_ref"],
            assigned["facts"]["task"]["task_ref"]
        );
        let replayed = f.deliver(assignment.clone(), false);
        assert_eq!(replayed["ok"], true, "{replayed}");
        assert_eq!(replayed["facts"]["receipt"], assigned["facts"]["receipt"]);
        assert_eq!(
            replayed["facts"]["task"]["task_ref"],
            assigned["facts"]["task"]["task_ref"]
        );
        let accepted_task =
            Uuid::parse_str(assigned["facts"]["task"]["task_ref"].as_str().unwrap()).unwrap();
        let mut writable = assignment;
        writable["idempotency_key"] = json!("write-denied");
        writable["assignment"]["accepted_task_identity"]["execution_intent"] =
            json!("canonical_workspace_write");
        let denied_task = f.deliver(writable, false);
        assert_eq!(denied_task["ok"], false, "{denied_task}");
        assert_eq!(
            denied_task["semantic"]["error"]["code"],
            "SPECIALIST_POLICY_DENIED"
        );
        assert_eq!(denied_task["semantic"]["error"]["effect"], "none");
        let mut another = spawn;
        another["idempotency_key"] = json!("spawn-two");
        let store = crate::engagement::EngagementStore::open(
            &crate::engagement::EngagementStore::workspace_database_path(&f.state()),
        )
        .unwrap();
        assert!(verify_accounted_task(store.connection(), accepted_task).unwrap());
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM brokered_tasks", [], |row| row
                    .get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(store.connection().query_row(
            "SELECT COUNT(*) FROM result_content_refs WHERE owner_kind='native_primary_task' AND representation='origin'",
            [], |row| row.get::<_, u32>(0)
        ).unwrap(), 1);
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM caller_scope_assignments", [], |row| {
                    row.get::<_, u32>(0)
                })
                .unwrap(),
            0
        );
        for state in ["provisioning", "active", "degraded", "retiring"] {
            store
                .connection()
                .execute("UPDATE brokered_members SET membership_state=?1", [state])
                .unwrap();
            let result = f.deliver(another.clone(), false);
            assert_eq!(result["ok"], false, "{state}: {result}");
            assert_eq!(
                result["semantic"]["error"]["code"],
                "SPECIALIST_POLICY_DENIED"
            );
            assert_eq!(
                result["semantic"]["error"]["required_action"],
                "fix_request"
            );
            assert_eq!(result["semantic"]["error"]["effect"], "none");
        }
        // Exercise the actual service's Case-A path after a guard/acceptance race:
        // a non-retired member has consumed the slot by transactional admission.
        let late = service::ManagedCallContext {
            session_id: f.manifest.run_id,
            source_run_id: f.manifest.run_id,
            source_turn_id: "root-turn",
            source_tool_call_id: "late-call",
            idempotency_key: "late-limit",
        };
        let refused = service::spawn_primary(
            &scope(&owner),
            &late,
            &crate::semantic::successor_fixture::AcceptingPort,
            service::SpawnSpecialist {
                provisioning_ref: candidate["provisioning_ref"].as_str().unwrap(),
                role_ref: "reviewer",
                agent_configuration_digest: candidate["agent_configuration_digest"]
                    .as_str()
                    .unwrap(),
                expected_revision: None,
            },
            &owner,
        )
        .unwrap_err();
        assert_eq!(refused.code, "SPECIALIST_POLICY_DENIED");
        assert_eq!(refused.details["effect"], "none");
        assert_eq!(refused.details["required_action"], "fix_request");
        let lookup = service::lookup_operation(
            &scope(&owner),
            "spawn_specialist",
            Some(("orchestrated_session", &f.manifest.run_id.to_string())),
            "late-limit",
        )
        .unwrap();
        assert_eq!(lookup["state"], "not_accepted");
        assert_eq!(
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM brokered_members", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        // Corrupt the actual retained proof, with an otherwise valid native owner.
        let prepared = std::fs::read_dir(&owner.root.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("prepared-")
            })
            .find(|path| {
                let proof: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                proof["request"] == request
            })
            .unwrap();
        let original = std::fs::read(&prepared).unwrap();
        let mut altered: Value = serde_json::from_slice(&original).unwrap();
        altered["owner"] = json!("foreign-owner");
        std::fs::write(&prepared, canonical(&altered).unwrap()).unwrap();
        assert_eq!(f.deliver(request.clone(), false)["ok"], false);
        std::fs::write(&prepared, b"invalid retained bytes").unwrap();
        assert_eq!(f.deliver(request, false)["ok"], false);
        std::fs::write(&prepared, original).unwrap();
        let denied = NativeFixture::new("user_approval_required");
        denied.boot().unwrap();
        let status = denied.deliver(json!({"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"context"}}), true);
        assert_eq!(status["ok"], true);
        assert_eq!(status["data"]["allowed_role_refs"], json!([]));
        let result = denied.deliver(install, false);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(
            result["semantic"]["error"]["required_action"], "fix_request",
            "{result}"
        );
    }

    #[test]
    fn delegated_owner_cannot_use_the_new_private_preparation_route() {
        let f = crate::trusted_host::tests::managed_fixture(HostRoute::Codex);
        let error = dispatch(&f.home, &f.owner(), &json!({"schema":SCHEMA,"operation":"prepare_provisioning","role_ref":"reviewer","idempotency_key":"original"})).unwrap_err();
        assert_eq!(error.code, "SPECIALIST_POLICY_DENIED");
        assert_eq!(error.details["required_action"], "fix_request");
    }

    #[test]
    fn preparation_is_private_managed_only_and_rejects_authority_fields() {
        let request = json!({"schema":SCHEMA,"operation":"prepare_provisioning","role_ref":"reviewer","idempotency_key":"original"});
        assert!(
            crate::host_invocation::normalize_tool_request(
                crate::native_consumer::MANAGED_NATIVE,
                &request
            )
            .is_ok()
        );
        for tool in [
            crate::native_consumer::EXTERNAL_NATIVE,
            crate::specialist_facade::MANAGED_TOOL,
            crate::native_consumer::MANAGED_CONTROL,
        ] {
            assert!(crate::host_invocation::normalize_tool_request(tool, &request).is_err());
        }
        for field in [
            "controller",
            "scope_ref",
            "owner_path",
            "profile_binding",
            "source_run_id",
            "run_generation",
        ] {
            let mut invalid = request.clone();
            invalid[field] = json!("forged");
            assert!(
                crate::host_invocation::normalize_tool_request(
                    crate::native_consumer::MANAGED_NATIVE,
                    &invalid
                )
                .is_err()
            );
        }
        let contract = crate::primary_tool::PrimaryToolContract::load()
            .unwrap()
            .with_specialists();
        let schema = contract.tool_specs()[1]["inputSchema"].clone();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("oneOf").is_none());
        assert!(
            schema["properties"]["operation"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("prepare_provisioning"))
        );
    }
    #[test]
    fn native_origin_uses_the_flattened_envelope_and_refuses_foreign_authority() {
        let f = crate::trusted_host::tests::Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        let root = Uuid::now_v7();
        let task = Uuid::now_v7();
        let operation = Uuid::now_v7();
        let member = Uuid::now_v7();
        let origin = Origin {
            root,
            workspace: owner.view.workspace_id.clone(),
            controller: owner.controller.clone(),
        };
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE orchestrated_sessions(session_id TEXT,workspace_id TEXT);
            CREATE TABLE brokered_tasks(task_id TEXT,session_id TEXT,source_run_id TEXT,request_json TEXT);
            CREATE TABLE delegation_operations(operation_ref TEXT,mutation_envelope_json TEXT);
            CREATE TABLE result_content(sha256 TEXT PRIMARY KEY,bytes BLOB NOT NULL);
            CREATE TABLE result_content_refs(owner_kind TEXT,owner_id TEXT,representation TEXT,sha256 TEXT,byte_length INTEGER,retired_operation TEXT,PRIMARY KEY(owner_kind,owner_id,representation));").unwrap();
        let ns = crate::operation::Namespace::delegation(
            &origin.controller.identity,
            &origin.workspace,
            Some(("orchestrated_session", &root.to_string())),
            "assign_specialist_task",
            "original",
        )
        .unwrap();
        let envelope = crate::operation::Envelope {
            schema: crate::operation::ENVELOPE_SCHEMA.into(),
            request_schema: "test".into(),
            namespace: ns,
            trusted_invocation_ref: None,
            request_body: crate::operation::BytesRef {
                byte_length: 4,
                sha256: format!("sha256:{}", crate::jcs::sha256_hex(b"task")),
            },
        };
        connection
            .execute(
                "INSERT INTO orchestrated_sessions VALUES(?1,?2)",
                rusqlite::params![root.to_string(), origin.workspace],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO brokered_tasks VALUES(?1,?2,?2,?3)",
                rusqlite::params![
                    task.to_string(),
                    root.to_string(),
                    json!({"operation_ref":operation}).to_string()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO delegation_operations VALUES(?1,?2)",
                rusqlite::params![
                    operation.to_string(),
                    serde_json::to_string(&envelope).unwrap()
                ],
            )
            .unwrap();
        let transaction = connection.unchecked_transaction().unwrap();
        origin
            .admit(
                &transaction,
                &crate::caller_scope::CallerAssignment {
                    assignment_id: task,
                    workspace_ref: &origin.workspace,
                    aggregate_scope_kind: "orchestrated_session",
                    aggregate_scope_ref: &root.to_string(),
                    member_ref: member,
                },
            )
            .unwrap();
        assert!(verify_accounted_task(&transaction, task).unwrap());
        let mut receipt = crate::delegation_identity::OperationReceipt {
            schema: "dolgorae.operation-receipt/v1".into(),
            operation_ref: operation.to_string(),
            namespace: (&envelope.namespace).into(),
            accepted_input_digest: format!("sha256:{}", "1".repeat(64)),
            accepted_subjects: crate::delegation_identity::AcceptedSubjects {
                task_ref: Some(task.to_string()),
                ..Default::default()
            },
            receipt_digest: String::new(),
            accepted_revision: 1,
            result_digest: None,
        };
        receipt.receipt_digest = receipt.compute_digest().unwrap();
        origin.verify_replay(&transaction, &receipt).unwrap();
        let mut other = origin.clone();
        other.controller.identity.generation += 1;
        assert!(other.verify_replay(&transaction, &receipt).is_err());
        receipt.accepted_subjects.task_ref = Some(Uuid::now_v7().to_string());
        assert!(origin.verify_replay(&transaction, &receipt).is_err());
        let mut foreign = envelope;
        foreign.namespace.controller_generation += 1;
        transaction
            .execute(
                "UPDATE delegation_operations SET mutation_envelope_json=?1",
                [serde_json::to_string(&foreign).unwrap()],
            )
            .unwrap();
        assert!(verify_accounted_task(&transaction, task).is_err());
    }
    #[test]
    fn complete_role_catalog_fits_its_bounded_native_metadata() {
        let roles = (0..64)
            .map(|index| format!("{index:02}{}", "r".repeat(62)))
            .collect::<Vec<_>>();
        let mut response = json!({"schema":"dolgorae.consumer-control-response/v1","action":"status","ok":true,"data":{"kind":"context","mode":"native_only","allowed_role_refs":roles}});
        assert!(crate::consumer_bridge::native_content(&response, true).is_ok());
        response["data"]["extra"] = json!("x".repeat(8192));
        assert!(crate::consumer_bridge::native_content(&response, true).is_err());
    }
    #[test]
    fn native_initialization_resumes_completed_stages_and_preserves_pending_state() {
        let f = crate::trusted_host::tests::Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        let generation = crate::host_invocation::preparation_generation(&owner).unwrap();
        crate::source_binding::initialize(&owner).unwrap();
        initialize(&owner).unwrap();
        let path = owner.root.path.join("consumer.sqlite");
        let original = std::fs::read(&path).unwrap();
        initialize(&owner).unwrap();
        assert_eq!(
            crate::host_invocation::preparation_generation(&owner).unwrap(),
            generation
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        owner.root.create_file("consumer.pending").unwrap();
        assert!(initialize(&owner).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    #[test]
    fn retained_controller_requires_original_authority_and_exact_private_bytes() {
        let f = crate::trusted_host::tests::Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        let carrier = crate::controller::CredentialCarrier::open_path(&f.controller).unwrap();
        carrier
            .retain_managed(&owner.root, &owner.controller)
            .unwrap();
        let original = std::fs::read(owner.root.path.join("controller")).unwrap();
        carrier
            .retain_managed(&owner.root, &owner.controller)
            .unwrap();
        let foreign = crate::trusted_host::tests::Fixture::new(HostRoute::Codex);
        let other = crate::controller::CredentialCarrier::open_path(&foreign.controller).unwrap();
        assert!(
            other
                .retain_managed(&owner.root, &owner.controller)
                .is_err()
        );
        assert_eq!(
            std::fs::read(owner.root.path.join("controller")).unwrap(),
            original
        );
    }
    #[test]
    fn complete_preparation_identities_are_not_limited_to_ordinary_metadata() {
        let contexts=(0..64).map(|index| json!({"kind":"accepted_context","identity":format!("{index:02}{}","c".repeat(126)),
            "content":crate::delegation_identity::BytesRef::from_bytes(b"context"),"media_type":"text/plain"})).collect::<Vec<_>>();
        let mut response = json!({"schema":RESPONSE_SCHEMA,"operation":"prepare_assignment","ok":true,"accepted_task_identity":{"contexts":contexts}});
        assert!(canonical(&response).unwrap().len() > 4096);
        assert!(crate::consumer_bridge::native_content(&response, true).is_ok());
        response["extra"] = json!("x".repeat(256 * 1024));
        assert!(crate::consumer_bridge::native_content(&response, true).is_err());
    }
    #[test]
    fn every_policy_axis_must_admit_the_readonly_role() {
        let mut input: Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/workspace-policy-input-v3.valid.json"
        ))
        .unwrap();
        input["approval_policy"] = json!("fully_delegated");
        input["roles"][0]["role_capture"] = json!(
            crate::instructions::RoleCapture::from_body("reviewer", "Read the selected task.")
                .unwrap()
        );
        let policy: crate::markdown_policy::PolicySnapshot = serde_json::from_value(input).unwrap();
        policy.validate().unwrap();
        assert!(eligible(&policy, &policy.roles[0]));
        for axis in 0..5 {
            let mut denied = policy.clone();
            match axis {
                0 => denied.approval_policy = "user_approval_required".into(),
                1 => denied.roles[0].primary_may_request = false,
                2 => denied.roles[0].auto_approve_when_fully_delegated = false,
                3 => denied.roles[0].activation_policy = "never".into(),
                _ => denied.roles[0].allowed_access = vec!["canonical_workspace_write".into()],
            }
            assert!(!eligible(&denied, &denied.roles[0]));
        }
    }
    #[test]
    fn retained_preparation_replays_exact_input_and_refuses_conflicts_and_corruption() {
        let f = crate::trusted_host::tests::Fixture::new(HostRoute::Codex);
        f.complete();
        let owner = f.owner();
        let request = json!({"schema":SCHEMA,"operation":"prepare_provisioning","role_ref":"reviewer","idempotency_key":"original"});
        assert!(
            retained(&owner, "prepared-test.json", &request)
                .unwrap()
                .is_none()
        );
        let record = Prepared {
            owner: owner.configuration_digest.clone(),
            request: request.clone(),
            response: json!({"original":true}),
        };
        owner
            .root
            .publish("prepared-test.json", &canonical(&record).unwrap())
            .unwrap();
        assert_eq!(
            retained(&owner, "prepared-test.json", &request).unwrap(),
            Some(record.response)
        );
        let mut changed = request.clone();
        changed["role_ref"] = json!("other");
        assert_eq!(
            retained(&owner, "prepared-test.json", &changed)
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        owner
            .root
            .create_file("prepared-test.json.pending")
            .unwrap();
        assert!(retained(&owner, "prepared-test.json", &request).is_err());
        assert!(owner.configuration.caller_scope.is_some());
    }
}

pub(crate) fn owner_path(state_root: &Path, root: Uuid) -> std::path::PathBuf {
    crate::run::run_root(state_root, root).join("native-specialist/owner.json")
}

/// Called within original creation startup, before any Worker can dispatch.
pub(crate) fn bootstrap(
    home: &crate::paths::DolgoraeHome,
    view: &crate::workspace::WorkspaceView,
    manifest: &crate::run::RunManifest,
    carrier: &crate::controller::CredentialCarrier,
) -> Result<(), MachineError> {
    let Some(aggregate) = &manifest.aggregate_binding else {
        return Ok(());
    };
    if aggregate.aggregate_kind != crate::domain::AggregateKind::OrchestratedSession
        || aggregate.member_kind != crate::run::AggregateMemberKind::Primary
        || aggregate.aggregate_id != manifest.run_id
        || manifest.schema_version != 4
    {
        return Ok(());
    }
    let state_root = home.workspace_root(&view.workspace_id);
    if crate::managed_specialist::has_explicit_connection(&state_root, manifest.run_id)? {
        crate::managed_specialist::load(&state_root, manifest.run_id)?;
        return Ok(());
    }
    let binding = manifest
        .global_profile_binding
        .as_ref()
        .ok_or_else(refusal)?;
    if !binding.retained_qualified_runtime_supported() {
        return Err(refusal());
    }
    let path = owner_path(&state_root, manifest.run_id);
    if !present(&path)? && worker_has_started(&state_root, manifest.run_id)? {
        // A previous producer may already have accepted a Thread without this
        // owner. Never attach new native authority to that accepted Thread.
        return Ok(());
    }
    let root_path = path.parent().ok_or_else(refusal)?;
    match std::fs::symlink_metadata(root_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::workspace::create_directory(root_path, 0o700).map_err(|_| refusal())?;
        }
        Err(_) => return Err(refusal()),
    }
    let root = PrivateDirectory::open(root_path)?;
    let native = root_path.join("native");
    match std::fs::symlink_metadata(&native) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::workspace::create_directory(&native, 0o700).map_err(|_| refusal())?;
        }
        Err(_) => return Err(refusal()),
    }
    root.verify_journal(&native)?;
    carrier.retain_managed(&root, &manifest.controller)?;
    let runtime = &binding.launch_snapshot;
    crate::trusted_host::create_owner(
        home,
        &view.canonical_path.to_path_buf()?,
        &path,
        OwnerDraft {
            route: HostRoute::Codex,
            executable: HostExecutable {
                path: runtime.executable_identity.resolved_path.clone().into(),
                version: runtime.codex_version.clone(),
                sha256: runtime.executable_identity.sha256.clone(),
            },
            owner: OwnerKind::Managed {
                session_ref: manifest.run_id,
            },
            controller_generation: manifest.controller.identity.generation,
            controller_file: root_path.join("controller"),
            native_host_root: native,
            allowed_scopes: vec![AllowedScope {
                kind: "orchestrated_session".into(),
                reference: Some(manifest.run_id.to_string()),
            }],
            tools: vec![
                crate::native_consumer::MANAGED_NATIVE.into(),
                crate::native_consumer::MANAGED_CONTROL.into(),
            ],
            caller_scope: None,
            adapter_id: "dolgorae.managed/1".into(),
            adapter_version: env!("CARGO_PKG_VERSION").into(),
        },
        carrier,
    )?;
    let owner = VerifiedOwner::load(home, &path)?;
    initialize(&owner)
}

fn present(path: &Path) -> Result<bool, MachineError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(refusal()),
    }
}

fn worker_has_started(state_root: &Path, run: Uuid) -> Result<bool, MachineError> {
    let runtime = crate::worker::runtime_record_path(&crate::worker::runtime_root(state_root), run)
        .map_err(|_| refusal())?;
    Ok(present(&runtime)?
        || present(&crate::worker::bootstrap_path(
            &crate::worker::runtime_root(state_root),
            run,
        ))?
        || present(&crate::run::run_root(state_root, run).join("thread-registrations"))?
        || crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, state_root)
            .load_state_projection(run)?
            .thread_id
            .is_some())
}

fn policy_denied(message: &str) -> MachineError {
    service::successor_error(
        "SPECIALIST_POLICY_DENIED",
        "native_policy",
        None,
        "none",
        "Rejected",
        "fix_request",
        message,
    )
}

fn initialize(owner: &VerifiedOwner) -> Result<(), MachineError> {
    crate::engagement::ensure_delegation_namespace(&owner.state_root)?;
    crate::host_invocation::initialize(owner)?;
    crate::source_binding::initialize(owner)?;
    crate::native_consumer::initialize(owner, crate::native_consumer::Mode::NativeOnly {})?;
    owner.revalidate()
}

fn captures(
    owner: &VerifiedOwner,
) -> Result<crate::markdown_policy::PreparedPolicySession, MachineError> {
    owner.revalidate()?;
    let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
        return Err(refusal());
    };
    let accepted = crate::orchestration::OrchestrationStore::open_observer(&owner.state_root)?
        .accepted_policy_session(session_ref)?;
    accepted
        .captures
        .validate_for_state_root(&owner.state_root)?;
    Ok(accepted.captures)
}

fn eligible(
    policy: &crate::markdown_policy::PolicySnapshot,
    role: &crate::markdown_policy::InstalledRole,
) -> bool {
    policy.approval_policy == "fully_delegated"
        && role.primary_may_request
        && role.auto_approve_when_fully_delegated
        && role.activation_policy != "never"
        && role
            .allowed_access
            .iter()
            .any(|access| access == "read_only")
}

pub(crate) fn managed_scope(owner: &VerifiedOwner) -> Value {
    match owner.configuration.owner {
        OwnerKind::Managed { session_ref } => {
            json!({"scope_kind":"orchestrated_session","scope_ref":session_ref})
        }
        OwnerKind::External => Value::Null,
    }
}
pub(crate) fn roles(owner: &VerifiedOwner) -> Result<Vec<String>, MachineError> {
    if owner.configuration.caller_scope.is_some() {
        return Ok(Vec::new());
    }
    let accepted = captures(owner)?;
    Ok(accepted
        .policy()
        .roles
        .iter()
        .filter(|role| eligible(accepted.policy(), role))
        .map(|role| role.role_ref.clone())
        .collect())
}

fn role(
    owner: &VerifiedOwner,
    reference: &str,
) -> Result<crate::markdown_policy::PreparedPolicySession, MachineError> {
    let accepted = captures(owner)?;
    if !accepted
        .policy()
        .roles
        .iter()
        .any(|role| role.role_ref == reference && eligible(accepted.policy(), role))
    {
        return Err(policy_denied(
            "the accepted Policy does not permit this read-only role",
        ));
    }
    Ok(accepted)
}

pub(crate) fn selection(
    owner: &VerifiedOwner,
    reference: &str,
) -> Result<crate::source_binding::ProvisioningSelection, MachineError> {
    let accepted = role(owner, reference)?;
    let child = accepted.child(reference)?;
    Ok(crate::source_binding::ProvisioningSelection {
        role_name: reference.into(),
        role: crate::instructions::SelectedInstructionSource::Inline(
            child.instructions.role.body.clone(),
        ),
        dependencies: Vec::new(),
        selected_guidance: Vec::new(),
        profile_binding: accepted.profile_binding(reference)?.clone(),
        admission: crate::provisioning::ProvisioningAdmission {
            model: child.model.clone(),
            effort: child.default_effort.clone(),
            purpose_kind: serde_json::to_value(child.purpose.kind)
                .map_err(|_| refusal())?
                .as_str()
                .ok_or_else(refusal)?
                .into(),
            purpose_label: child.purpose.external_label.clone(),
            required_capabilities: child.required_capabilities.clone(),
            execution_lane: "dedicated".into(),
            requested_access: "read_only".into(),
            required_assurance: "best_effort_personal_alpha".into(),
            native_subagent_policy: child.native_subagent_policy.clone(),
        },
    })
}

pub(crate) fn guard(owner: &VerifiedOwner, request: &Value) -> Result<(), MachineError> {
    if owner.configuration.caller_scope.is_some() {
        return Ok(());
    }
    let operation = request["operation"].as_str().ok_or_else(refusal)?;
    let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
        return Err(refusal());
    };
    if [
        "install_provisioning",
        "spawn_specialist",
        "assign_specialist_task",
    ]
    .contains(&operation)
    {
        let root = session_ref.to_string();
        let aggregate = (operation != "install_provisioning")
            .then_some(("orchestrated_session", root.as_str()));
        let lookup = service::lookup_operation(
            &scope(owner),
            operation,
            aggregate,
            text(request, "idempotency_key")?,
        )?;
        if lookup["state"] == "accepted" || lookup["state"] == "unresolved" {
            return Ok(());
        }
    }
    match operation {
        "prepare_provisioning" => {
            role(owner, text(request, "role_ref")?)?;
        }
        "validate_provisioning" | "install_provisioning" => {
            let sources: crate::provisioning::ProvisioningSourcesIdentity =
                serde_json::from_value(request["sources"].clone()).map_err(|_| refusal())?;
            let mut matched = false;
            for reference in roles(owner)? {
                let selected = selection(owner, &reference)?;
                let current = crate::source_binding::capture_selection(&home(owner)?, &selected)?;
                if current.sources_identity()? == sources {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return Err(policy_denied(
                    "the accepted Policy does not permit this provisioning",
                ));
            }
        }
        "spawn_specialist" => {
            let reference = text(request, "role_ref")?;
            let accepted = role(owner, reference)?;
            let snapshot = crate::provisioning::retained_snapshot(
                &owner.state_root,
                text(request, "provisioning_ref")?,
            )?;
            let selected = selection(owner, reference)?;
            if snapshot != crate::source_binding::capture_selection(&home(owner)?, &selected)?
                || snapshot.admitted.requested_access != "read_only"
            {
                return Err(refusal());
            }
            let store = crate::engagement::EngagementStore::open_read_only(
                &crate::engagement::EngagementStore::workspace_database_path(&owner.state_root),
            )?;
            let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
                return Err(refusal());
            };
            let (all, same): (u32, u32) = store.connection().query_row(
                "SELECT COUNT(*),COALESCE(SUM(role_ref=?2),0) FROM brokered_members WHERE session_id=?1 AND membership_state<>'retired'",
                rusqlite::params![session_ref.to_string(), reference], |row| Ok((row.get(0)?,row.get(1)?))
            ).map_err(|_| refusal())?;
            let policy_role = accepted
                .policy()
                .roles
                .iter()
                .find(|role| role.role_ref == reference)
                .ok_or_else(refusal)?;
            if all >= accepted.policy().max_active_specialists
                || same >= policy_role.max_active_instances
            {
                return Err(policy_denied(
                    "the accepted Policy instance limit is reached",
                ));
            }
        }
        "prepare_assignment" | "assign_specialist_task" => {
            let member = uuid(request, "member_ref")?;
            let configuration =
                service::preparation_configuration(&scope(owner), managed_owner(owner)?, member)?;
            role(
                owner,
                configuration
                    .role_reference
                    .as_deref()
                    .ok_or_else(refusal)?,
            )?;
            if operation == "assign_specialist_task"
                && request["assignment"]["accepted_task_identity"]["execution_intent"]
                    != "read_only"
            {
                return Err(policy_denied(
                    "native-only assignment requires read-only access",
                ));
            }
        }
        "await_specialist_task"
        | "collect_specialist_task"
        | "cancel_specialist_task"
        | "release_specialist"
        | "lookup_operation" => {}
        _ => return Err(refusal()),
    }
    Ok(())
}
fn home(owner: &VerifiedOwner) -> Result<crate::paths::DolgoraeHome, MachineError> {
    Ok(crate::paths::DolgoraeHome::from_canonical_home(
        owner
            .state_root
            .ancestors()
            .nth(3)
            .ok_or_else(refusal)?
            .into(),
    )?)
}
fn scope(owner: &VerifiedOwner) -> DelegationScope<'_> {
    DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    }
}
fn managed_owner(owner: &VerifiedOwner) -> Result<service::SettlementOwner, MachineError> {
    let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
        return Err(refusal());
    };
    Ok(service::SettlementOwner::Managed {
        session_id: session_ref,
    })
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, MachineError> {
    value[key].as_str().ok_or_else(refusal)
}
fn uuid(value: &Value, key: &str) -> Result<Uuid, MachineError> {
    Uuid::parse_str(text(value, key)?).map_err(|_| refusal())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    owner: String,
    request: Value,
    response: Value,
}
fn retained(
    owner: &VerifiedOwner,
    name: &str,
    request: &Value,
) -> Result<Option<Value>, MachineError> {
    let _lock = owner.root.lock()?;
    let path = owner.root.path.join(name);
    if std::fs::symlink_metadata(owner.root.path.join(format!("{name}.pending"))).is_ok() {
        return Err(refusal());
    }
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let old: Prepared = owner.root.read_bounded(
                &path,
                (crate::specialist_facade::PUBLIC_LIMIT * 2 + 65_536) as u64,
            )?;
            if !owner.matches_persisted_owner_digest(&old.owner) {
                return Err(refusal());
            }
            if old.request != *request {
                return Err(crate::trusted_host::conflict());
            }
            Ok(Some(old.response))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(refusal()),
    }
}
pub(crate) fn dispatch(
    home: &crate::paths::DolgoraeHome,
    owner: &VerifiedOwner,
    request: &Value,
) -> Result<Value, MachineError> {
    if owner.configuration.caller_scope.is_some() {
        return Err(policy_denied(
            "private preparation requires an automatic native-only owner",
        ));
    }
    let operation = text(request, "operation")?;
    let key = text(request, "idempotency_key")?;
    let operation_kind = if operation == "prepare_provisioning" {
        "install_provisioning"
    } else {
        "assign_specialist_task"
    };
    let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
        return Err(refusal());
    };
    let root_ref = session_ref.to_string();
    let aggregate = (operation_kind != "install_provisioning")
        .then_some(("orchestrated_session", root_ref.as_str()));
    let ns = crate::operation::Namespace::delegation(
        &owner.controller.identity,
        &owner.view.workspace_id,
        aggregate,
        operation_kind,
        key,
    )
    .map_err(|_| refusal())?;
    let name = format!("prepared-{}.json", crate::jcs::sha256_hex(&canonical(&ns)?));
    // Original operation and retained input precede fresh source capture.
    let lookup = service::lookup_operation(&scope(owner), operation_kind, aggregate, key)?;
    if let Some(mut response) = retained(owner, &name, request)? {
        if lookup["state"] == "accepted" || lookup["state"] == "unresolved" {
            response["lookup"] = lookup;
        }
        return Ok(response);
    }
    if lookup["state"] != "absent" && lookup["state"] != "not_accepted" {
        return Ok(
            json!({"schema":RESPONSE_SCHEMA,"operation":operation,"ok":true,"scope":managed_scope(owner),"lookup":lookup}),
        );
    }
    guard(owner, request)?;
    let (field, projection) = if operation == "prepare_provisioning" {
        let reference = text(request, "role_ref")?;
        let selected = selection(owner, reference)?;
        (
            "sources",
            serde_json::to_value(crate::source_binding::prepare_provisioning(
                home, owner, selected,
            )?)
            .map_err(|_| refusal())?,
        )
    } else {
        let task = serde_json::from_value(request["task_body"].clone()).map_err(|_| refusal())?;
        let selected = crate::source_binding::AssignmentSelection {
            namespace: (&ns).into(),
            member_ref: uuid(request, "member_ref")?,
            task,
            execution_intent: "read_only".into(),
            deadline_seconds: request["deadline_seconds"].as_u64().ok_or_else(refusal)?,
            target: None,
        };
        (
            "accepted_task_identity",
            serde_json::to_value(crate::source_binding::prepare_assignment(owner, selected)?)
                .map_err(|_| refusal())?,
        )
    };
    let response = json!({"schema":RESPONSE_SCHEMA,"operation":operation,"ok":true,"scope":managed_scope(owner),field:projection});
    let record = Prepared {
        owner: owner.configuration_digest.clone(),
        request: request.clone(),
        response: response.clone(),
    };
    // Recapture work is outside the lock; publication compares a concurrent winner.
    if let Some(existing) = retained(owner, &name, request)? {
        return Ok(existing);
    }
    let _lock = owner.root.lock()?;
    owner.revalidate()?;
    match std::fs::symlink_metadata(owner.root.path.join(&name)) {
        Ok(_) => {
            let old: Prepared = owner.root.read_bounded(
                &owner.root.path.join(&name),
                (crate::specialist_facade::PUBLIC_LIMIT * 2 + 65_536) as u64,
            )?;
            if !owner.matches_persisted_owner_digest(&old.owner)
                || old.request != record.request
                || old.response != response
            {
                return Err(crate::trusted_host::conflict());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            owner.root.publish(&name, &canonical(&record)?)?
        }
        Err(_) => return Err(refusal()),
    }
    Ok(response)
}

/// Re-read limits inside the spawn acceptance transaction, before membership.
pub(crate) fn admit_spawn(
    connection: &rusqlite::Connection,
    scope: &DelegationScope<'_>,
    context: &service::ManagedCallContext<'_>,
    reference: &str,
    owner: &VerifiedOwner,
) -> Result<(), MachineError> {
    owner.revalidate()?;
    if owner.configuration.caller_scope.is_some()
        || crate::managed_specialist::has_explicit_connection(scope.state_root, context.session_id)?
    {
        return Err(service::assignment_refusal(
            "SPECIALIST_POLICY_DENIED",
            "automatic native admission no longer owns this connection",
        ));
    }
    if owner.controller != *scope.controller {
        return Err(service::assignment_refusal(
            "AUTHORIZATION_FAILED",
            "native admission differs from the accepted Controller",
        ));
    }
    let accepted = role(owner, reference).map_err(|mut error| {
        if error.code == "SPECIALIST_POLICY_DENIED" {
            error.details["assignment_refusal"] = json!(true);
        }
        error
    })?;
    let (all, same): (u32,u32) = connection.query_row(
        "SELECT COUNT(*),COALESCE(SUM(role_ref=?2),0) FROM brokered_members WHERE session_id=?1 AND membership_state<>'retired'",
        rusqlite::params![context.session_id.to_string(),reference],
        |row| Ok((row.get(0)?,row.get(1)?)),
    ).map_err(|_| refusal())?;
    let selected = accepted
        .policy()
        .roles
        .iter()
        .find(|role| role.role_ref == reference)
        .ok_or_else(refusal)?;
    if all >= accepted.policy().max_active_specialists || same >= selected.max_active_instances {
        return Err(service::assignment_refusal(
            "SPECIALIST_POLICY_DENIED",
            "the accepted Policy instance limit is reached",
        ));
    }
    Ok(())
}

/// Real managed Primary origin, independent of Podway's five-reference budget.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Origin {
    root: Uuid,
    workspace: String,
    controller: crate::run::ControllerBinding,
}
impl Origin {
    pub(crate) fn from_owner(owner: &VerifiedOwner) -> Result<Self, MachineError> {
        owner.revalidate()?;
        if owner.configuration.caller_scope.is_some() {
            return Err(refusal());
        }
        let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
            return Err(refusal());
        };
        Ok(Self {
            root: session_ref,
            workspace: owner.view.workspace_id.clone(),
            controller: owner.controller.clone(),
        })
    }
    pub(crate) fn admit(
        &self,
        connection: &rusqlite::Connection,
        task: &crate::caller_scope::CallerAssignment<'_>,
    ) -> Result<(), MachineError> {
        if connection.is_autocommit()
            || task.workspace_ref != self.workspace
            || task.aggregate_scope_kind != "orchestrated_session"
            || task.aggregate_scope_ref != self.root.to_string()
        {
            return Err(refusal());
        }
        crate::storage_observation::put_content(
            connection,
            "native_primary_task",
            &task.assignment_id.to_string(),
            "origin",
            &canonical(self)?,
        )?;
        if !verify_accounted_task(connection, task.assignment_id)? {
            return Err(refusal());
        }
        Ok(())
    }
    pub(crate) fn verify_replay(
        &self,
        connection: &rusqlite::Connection,
        receipt: &crate::delegation_identity::OperationReceipt,
    ) -> Result<(), MachineError> {
        let task = receipt
            .accepted_subjects
            .task_ref
            .as_deref()
            .ok_or_else(refusal)?;
        let task = Uuid::parse_str(task).map_err(|_| refusal())?;
        let retained = crate::storage_observation::read_content(
            connection,
            "native_primary_task",
            &task.to_string(),
            "origin",
        )?
        .ok_or_else(refusal)?;
        if serde_json::from_slice::<Self>(&retained).map_err(|_| refusal())? != *self
            || !verify_accounted_task(connection, task)?
        {
            return Err(refusal());
        }
        Ok(())
    }
}
pub(crate) fn verify_accounted_task(
    connection: &rusqlite::Connection,
    task: Uuid,
) -> Result<bool, MachineError> {
    let Some(bytes) = crate::storage_observation::read_content(
        connection,
        "native_primary_task",
        &task.to_string(),
        "origin",
    )?
    else {
        return Ok(false);
    };
    let origin: Origin = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
    let (root, workspace, envelope): (String,String,String) = connection.query_row(
        "SELECT t.session_id,s.workspace_id,d.mutation_envelope_json FROM brokered_tasks t JOIN orchestrated_sessions s ON s.session_id=t.session_id JOIN delegation_operations d ON d.operation_ref=json_extract(t.request_json,'$.operation_ref') WHERE t.task_id=?1 AND t.source_run_id=t.session_id",
        [task.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))
    ).map_err(|_| refusal())?;
    let envelope: crate::operation::Envelope =
        serde_json::from_str(&envelope).map_err(|_| refusal())?;
    let ns = crate::operation::Namespace::delegation(
        &origin.controller.identity,
        &origin.workspace,
        Some(("orchestrated_session", &root)),
        "assign_specialist_task",
        &envelope.namespace.idempotency_key,
    )
    .map_err(|_| refusal())?;
    if root != origin.root.to_string() || workspace != origin.workspace || envelope.namespace != ns
    {
        return Err(refusal());
    }
    Ok(true)
}
