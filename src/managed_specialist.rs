//! Separate successor dispatch from the authenticated Root Worker boundary.
use crate::delegation_service::{DelegationScope, ManagedCallContext};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::specialist_facade as facade;
use crate::trusted_host::{OwnerKind, PrivateDirectory, VerifiedOwner, canonical, refusal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;
const CONNECTION: &str = "specialist-owner.json";
#[derive(Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Connection {
    schema: String,
    root_run: Uuid,
    owner_path: PathBuf,
    owner_digest: String,
}
pub(crate) fn has_explicit_connection(state_root: &Path, root: Uuid) -> Result<bool, MachineError> {
    match std::fs::symlink_metadata(crate::run::run_root(state_root, root).join(CONNECTION)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(refusal()),
    }
}
pub(crate) fn connect(owner: &VerifiedOwner) -> Result<(), MachineError> {
    owner.revalidate()?;
    let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
        return Err(refusal());
    };
    let root = PrivateDirectory::open(&crate::run::run_root(&owner.state_root, session_ref))?;
    let record = Connection {
        schema: "dolgorae.managed-owner-connection/v1".into(),
        root_run: session_ref,
        owner_path: owner.path.clone(),
        owner_digest: owner.configuration_digest.clone(),
    };
    let path = root.path.join(CONNECTION);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let existing: Connection = root.read(&path)?;
            if existing != record {
                return Err(refusal());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            root.publish(CONNECTION, &canonical(&record)?)?
        }
        Err(_) => return Err(refusal()),
    }
    owner.revalidate()
}
pub(crate) fn load(
    state_root: &Path,
    root_run: Uuid,
) -> Result<(DolgoraeHome, VerifiedOwner), MachineError> {
    let root = PrivateDirectory::open(&crate::run::run_root(state_root, root_run))?;
    if !has_explicit_connection(state_root, root_run)? {
        let physical_home = state_root.ancestors().nth(3).ok_or_else(refusal)?;
        let home = DolgoraeHome::from_canonical_home(physical_home.to_owned())?;
        let owner = VerifiedOwner::load(
            &home,
            &crate::native_preparation::owner_path(state_root, root_run),
        )?;
        if owner.state_root != state_root
            || owner.configuration.owner
                != (OwnerKind::Managed {
                    session_ref: root_run,
                })
            || owner.configuration.caller_scope.is_some()
        {
            return Err(refusal());
        }
        return Ok((home, owner));
    }
    let record: Connection = root.read(&root.path.join(CONNECTION))?;
    if record.schema != "dolgorae.managed-owner-connection/v1" || record.root_run != root_run {
        return Err(refusal());
    }
    let physical_home = state_root.ancestors().nth(3).ok_or_else(refusal)?;
    let home =
        DolgoraeHome::from_canonical_home(physical_home.to_owned()).map_err(|_| refusal())?;
    let owner = VerifiedOwner::load(&home, &record.owner_path)?;
    if owner.state_root != state_root
        || owner.configuration.owner
            != (OwnerKind::Managed {
                session_ref: root_run,
            })
        || owner.configuration_digest != record.owner_digest
    {
        return Err(refusal());
    }
    Ok((home, owner))
}
fn bridge<'a>(
    owner: &'a VerifiedOwner,
    call: &'a crate::turn::PrimaryToolCall,
    root: Uuid,
    generation: u64,
    port: &'a dyn crate::delegation_service::RunSettlementPort,
) -> Result<crate::consumer_bridge::Bridge<'a>, MachineError> {
    let witness = crate::native_consumer::Witness::managed(owner, call, root, generation)?;
    Ok(crate::consumer_bridge::Bridge {
        store: crate::native_consumer::Store::open(owner, witness)?,
        source: crate::consumer_bridge::ProductSource {
            owner,
            port,
            managed: Some(ManagedCallContext {
                session_id: root,
                source_run_id: root,
                source_turn_id: &call.context.source_turn_id,
                source_tool_call_id: &call.context.source_tool_call_id,
                idempotency_key: &call.context.idempotency_key,
            }),
        },
    })
}
fn control(
    state_root: &Path,
    call: &crate::turn::PrimaryToolCall,
    root: Uuid,
    generation: u64,
    workspace: &str,
) -> Value {
    let result = (|| {
        let (_, owner) = load(state_root, root)?;
        if owner.view.workspace_id != workspace {
            return Err(refusal());
        }
        let carrier =
            crate::controller::CredentialCarrier::open_path(&owner.configuration.controller_file)
                .map_err(|_| refusal())?;
        let port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
        let bridge = bridge(&owner, call, root, generation, &port)?;
        let control = crate::native_consumer::Control::parse(&crate::native_consumer::encode(
            &call.payload,
        )?)?;
        Ok(bridge.handle(&mut crate::podway_client::PublicCli, &control))
    })();
    result.unwrap_or_else(|e| facade::control_failure(&call.payload, &e))
}

/// Called only by the Worker after native Thread/Turn/call correlation and its
/// own generation fence. Public strings select operations, never authority.
pub(crate) fn dispatch(
    state_root: &Path,
    call: &crate::turn::PrimaryToolCall,
    root_run: Uuid,
    run_generation: u64,
    workspace: &str,
) -> Value {
    dispatch_in(state_root, call, root_run, run_generation, workspace, None)
}

#[cfg(test)]
pub(crate) fn dispatch_with_port(
    state_root: &Path,
    call: &crate::turn::PrimaryToolCall,
    root_run: Uuid,
    run_generation: u64,
    workspace: &str,
    port: &dyn crate::delegation_service::RunSettlementPort,
) -> Value {
    dispatch_in(
        state_root,
        call,
        root_run,
        run_generation,
        workspace,
        Some(port),
    )
}

fn dispatch_in(
    state_root: &Path,
    call: &crate::turn::PrimaryToolCall,
    root_run: Uuid,
    run_generation: u64,
    workspace: &str,
    supplied_port: Option<&dyn crate::delegation_service::RunSettlementPort>,
) -> Value {
    if call.tool_name == crate::native_consumer::MANAGED_CONTROL {
        return control(state_root, call, root_run, run_generation, workspace);
    }
    let native = call.tool_name == crate::native_consumer::MANAGED_NATIVE;
    let mut native_owner = None;
    let mut post_error = None;
    let request = &call.payload;
    let operation = request["operation"].as_str().unwrap_or("lookup_operation");
    let mut entered = false;
    let result = (|| {
        if !crate::native_consumer::managed_tool(&call.tool_name)
            || call.run_generation != run_generation
            || call.context.source_run_id != root_run
            || call.context.session_id != root_run
            || call.source_thread_id.is_empty()
        {
            return Err(refusal());
        }
        crate::host_invocation::normalize_tool_request(&call.tool_name, request)?;
        let preparation = native && request["schema"] == crate::native_preparation::SCHEMA;
        if !preparation && !facade::MANAGED.contains(&operation) {
            return Err(refusal());
        }
        let (home, owner) = load(state_root, root_run)?;
        if owner.view.workspace_id != workspace {
            return Err(refusal());
        }
        let root_ref = root_run.to_string();
        if request.get("scope_kind").is_some()
            && (request["scope_kind"] != "orchestrated_session" || request["scope_ref"] != root_ref)
        {
            return Err(refusal());
        }
        if operation == "lookup_operation" {
            let ns: crate::delegation_identity::LookupNamespace =
                serde_json::from_value(request["namespace"].clone()).map_err(|_| refusal())?;
            let scoped = ns.scope_kind.as_deref().zip(ns.scope_ref.as_deref());
            if !match ns.operation_kind.as_str() {
                "install_provisioning" => scoped.is_none(),
                "spawn_specialist"
                | "assign_specialist_task"
                | "assign_group"
                | "merge_group_results"
                | "assign_synthesis" => scoped == Some(("orchestrated_session", root_ref.as_str())),
                _ => false,
            } {
                return Err(refusal());
            }
            let expected = crate::operation::Namespace::delegation(
                &owner.controller.identity,
                &owner.view.workspace_id,
                scoped,
                &ns.operation_kind,
                &ns.idempotency_key,
            )
            .map_err(|_| refusal())?;
            if ns != crate::delegation_identity::LookupNamespace::from(&expected) {
                return Err(refusal());
            }
        }

        if native {
            let carrier = crate::controller::CredentialCarrier::open_path(
                &owner.configuration.controller_file,
            )?;
            let port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
            bridge(&owner, call, root_run, run_generation, &port)?;
            owner.permit(Some(("orchestrated_session", &root_ref)))?;
        }
        if preparation {
            return crate::native_preparation::dispatch(&home, &owner, request);
        }
        facade::typed_preflight(request)?;
        crate::native_preparation::guard(&owner, request)?;
        let key = request["idempotency_key"]
            .as_str()
            .unwrap_or(&call.context.idempotency_key);
        let context = ManagedCallContext {
            session_id: root_run,
            source_run_id: root_run,
            source_turn_id: &call.context.source_turn_id,
            source_tool_call_id: &call.context.source_tool_call_id,
            idempotency_key: key,
        };
        let scope = DelegationScope {
            view: &owner.view,
            state_root,
            controller: &owner.controller,
            trusted_invocation: None,
        };
        let carrier =
            crate::controller::CredentialCarrier::open_path(&owner.configuration.controller_file)
                .map_err(|_| refusal())?;
        let semantic_port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
        let port = supplied_port.unwrap_or(&semantic_port);
        let admission = || {
            bridge(&owner, call, root_run, run_generation, port)?
                .assignment_binding(&mut crate::podway_client::PublicCli, request)
        };
        entered = true;
        let result = facade::dispatch(
            &home,
            &owner,
            &scope,
            Some(&context),
            port,
            request,
            native.then_some(&admission),
        )?;
        if native {
            post_error = owner.revalidate().err();
            native_owner = Some(owner);
        } else {
            owner.revalidate()?;
        }
        Ok(result)
    })();
    if let Some(correlation) =
        crate::native_preparation::Correlation::from_request(&call.tool_name, request)
    {
        return result.unwrap_or_else(|error| correlation.failure(&error));
    }
    let public = facade::finish(operation, result, entered);
    if !native {
        return public;
    }
    let mut presented = crate::consumer_bridge::semantic_presentation(&public);
    if let Some(owner) = native_owner.filter(|_| {
        public.get("identity").is_some() || public["task"]["complete_result"].is_object()
    }) {
        let material: Result<Value, MachineError> = (|| {
            let carrier = crate::controller::CredentialCarrier::open_path(
                &owner.configuration.controller_file,
            )
            .map_err(|_| refusal())?;
            let port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
            Ok(
                bridge(&owner, call, root_run, run_generation, &port)?.semantic(
                    &mut crate::podway_client::PublicCli,
                    request,
                    &public,
                ),
            )
        })();
        match material {
            Ok(value) => presented = value,
            Err(e) => post_error = Some(e),
        }
    }
    if let Some(e) = post_error {
        presented["ok"] = serde_json::json!(false);
        presented["presentation_error"] = serde_json::json!({"code":e.code,"recovery":"inspect original operation; do not redispatch"});
    }
    presented
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trusted_host::{HostRoute, tests::managed_fixture};
    #[test]
    fn explicitly_connected_owner_keeps_the_original_native_only_context_shape() {
        use std::os::unix::fs::PermissionsExt;
        let f = managed_fixture(HostRoute::Codex);
        let owner = f.owner();
        let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
            panic!()
        };
        connect(&owner).unwrap();
        crate::native_consumer::initialize(&owner, crate::native_consumer::Mode::NativeOnly {})
            .unwrap();
        let mut projection = crate::projection::RunStateProjection::starting(session_ref);
        projection.run_generation = 1;
        let path = crate::run::run_root(&owner.state_root, session_ref).join("state.json");
        std::fs::write(&path, canonical(&projection).unwrap()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let call = crate::turn::PrimaryToolCall {
            tool_name: crate::native_consumer::MANAGED_CONTROL.into(),
            source_thread_id: "root-thread".into(),
            request_id: 1,
            run_generation: 1,
            context: crate::primary_tool::PrimaryCallContext {
                session_id: session_ref,
                source_run_id: session_ref,
                source_turn_id: "root-turn".into(),
                source_tool_call_id: "root-call".into(),
                idempotency_key: "context".into(),
            },
            payload: serde_json::json!({"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"context"}}),
        };
        let observed = dispatch_with_port(
            &owner.state_root,
            &call,
            session_ref,
            1,
            &owner.view.workspace_id,
            &crate::semantic::successor_fixture::AcceptingPort,
        );
        assert_eq!(observed["ok"], true, "{observed}");
        assert_eq!(
            observed["data"],
            serde_json::json!({"kind":"context","mode":"native_only","context_ref":null})
        );
    }

    #[test]
    fn preparation_refusals_use_the_private_response_contract() {
        let error = crate::native_preparation::invalid_request();
        let correlation = crate::native_preparation::Correlation::from_request(
            crate::native_consumer::MANAGED_NATIVE,
            &serde_json::json!({"schema":crate::native_preparation::SCHEMA,"operation":"prepare_provisioning"}),
        ).unwrap();
        let response = correlation.failure(&error);
        assert_eq!(response["error"]["required_action"], "fix_request");
        let schemas = crate::host_invocation::request_schemas();
        assert!(crate::host_invocation::schema_matches(
            &response,
            &schemas[4]["$defs"]["response"],
            &schemas[4],
            0
        ));
        assert!(!crate::host_invocation::schema_matches(
            &response,
            &schemas[0]["$defs"]["failure"],
            &schemas[0],
            0
        ));
        assert!(
            !crate::consumer_bridge::native_content(&response, true)
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn managed_connection_is_explicit_immutable_and_current_root_owned() {
        for route in [HostRoute::Codex, HostRoute::Claude] {
            let f = managed_fixture(route);
            let owner = f.owner();
            let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
                panic!()
            };
            assert!(load(&owner.state_root, session_ref).is_err());
            connect(&owner).unwrap();
            connect(&owner).unwrap();
            let (_, loaded) = load(&owner.state_root, session_ref).unwrap();
            assert_eq!(loaded.configuration_digest, owner.configuration_digest);
            assert!(load(&owner.state_root, Uuid::now_v7()).is_err());
            let root =
                PrivateDirectory::open(&crate::run::run_root(&owner.state_root, session_ref))
                    .unwrap();
            let path = root.path.join(CONNECTION);
            let bytes = std::fs::read(&path).unwrap();
            for field in ["root_run", "owner_digest", "owner_path", "schema"] {
                let mut value: Value = serde_json::from_slice(&bytes).unwrap();
                value[field] = serde_json::json!(if field == "root_run" {
                    Uuid::now_v7().to_string()
                } else {
                    "foreign".into()
                });
                std::fs::write(&path, canonical(&value).unwrap()).unwrap();
                assert!(
                    load(&owner.state_root, session_ref).is_err(),
                    "changed {field}"
                );
                assert!(connect(&owner).is_err(), "changed connection was replaced");
            }
            std::fs::write(&path, &bytes).unwrap();
            let (_, loaded) = load(&owner.state_root, session_ref).unwrap();
            assert_eq!(loaded.path, f.path());
            let foreign = crate::trusted_host::tests::Fixture::new(HostRoute::Codex);
            foreign.complete();
            assert!(connect(&foreign.owner()).is_err());
        }
    }
    #[test]
    fn managed_worker_source_and_public_scope_are_separate_authority_axes() {
        let f = managed_fixture(HostRoute::Codex);
        let owner = f.owner();
        let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
            panic!()
        };
        connect(&owner).unwrap();
        let ns = crate::operation::Namespace::delegation(
            &owner.controller.identity,
            &owner.view.workspace_id,
            Some(("orchestrated_session", &session_ref.to_string())),
            "spawn_specialist",
            "not-used",
        )
        .unwrap();
        let call = crate::turn::PrimaryToolCall {
            tool_name: facade::MANAGED_TOOL.into(),
            source_thread_id: "real-thread".into(),
            request_id: 1,
            run_generation: 2,
            context: crate::primary_tool::PrimaryCallContext {
                session_id: session_ref,
                source_run_id: session_ref,
                source_turn_id: "real-turn".into(),
                source_tool_call_id: "real-call".into(),
                idempotency_key: "native-context-key".into(),
            },
            payload: serde_json::json!({"schema":"dolgorae.external-specialist-request/v5","operation":"lookup_operation","namespace":crate::delegation_identity::LookupNamespace::from(&ns)}),
        };
        let result = dispatch(
            &owner.state_root,
            &call,
            session_ref,
            2,
            &owner.view.workspace_id,
        );
        assert_eq!(
            result["schema"], "dolgorae.external-specialist-response/v5",
            "{result}"
        );
        assert_eq!(result["lookup"]["state"], "absent");
        for axis in 0..9 {
            let mut changed = call.clone();
            match axis {
                0 => changed.tool_name = "dolgorae_orchestration".into(),
                1 => changed.run_generation = 3,
                2 => changed.context.session_id = Uuid::now_v7(),
                3 => changed.context.source_run_id = Uuid::now_v7(),
                4 => changed.source_thread_id.clear(),
                5 => changed.payload["namespace"]["scope_ref"] = serde_json::json!(Uuid::now_v7()),
                6 => {
                    changed.payload["namespace"]["controller_principal"] =
                        serde_json::json!("foreign")
                }
                7 => changed.payload["namespace"]["controller_generation"] = serde_json::json!(2),
                _ => changed.payload["namespace"]["workspace_ref"] = serde_json::json!("foreign"),
            };
            let result = dispatch(
                &owner.state_root,
                &changed,
                session_ref,
                2,
                &owner.view.workspace_id,
            );
            assert_eq!(
                result["schema"], "dolgorae.external-specialist-failure/v5",
                "axis {axis}: {result}"
            );
            assert_eq!(result["error"]["effect"], "none");
        }
        for field in ["member_ref", "task_ref"] {
            let mut changed = call.clone();
            let operation = if field == "task_ref" {
                "cancel_specialist_task"
            } else {
                "release_specialist"
            };
            changed.payload = serde_json::json!({"schema":"dolgorae.external-specialist-request/v5","operation":operation,"scope_kind":"orchestrated_session","scope_ref":session_ref,"member_ref":Uuid::now_v7(),"expected_revision":0});
            if field == "task_ref" {
                changed.payload["task_ref"] = serde_json::json!(Uuid::now_v7());
                changed.payload["reason"] = serde_json::json!("invalid reference");
            }
            changed.payload[field] = serde_json::json!("not-a-retained-uuid");
            let result = dispatch(
                &owner.state_root,
                &changed,
                session_ref,
                2,
                &owner.view.workspace_id,
            );
            assert_eq!(
                result["error"]["effect"], "none",
                "invalid {field}: {result}"
            );
        }
        let result = dispatch(
            &owner.state_root,
            &call,
            session_ref,
            2,
            "foreign-workspace",
        );
        assert_eq!(result["error"]["effect"], "none");
        assert!(
            facade::validates(&result, "failure")
                || result["schema"] == "dolgorae.external-specialist-failure/v5"
        );
    }
    #[test]
    fn native_managed_control_uses_root_generation_and_call_authority_without_consumer() {
        let f = managed_fixture(HostRoute::Codex);
        let owner = f.owner();
        let OwnerKind::Managed { session_ref } = owner.configuration.owner else {
            panic!()
        };
        connect(&owner).unwrap();
        crate::native_consumer::initialize(&owner, crate::native_consumer::Mode::NativeOnly {})
            .unwrap();
        let state = crate::run::run_root(&owner.state_root, session_ref).join("state.json");
        let mut projection = crate::projection::RunStateProjection::starting(session_ref);
        projection.run_generation = 1;
        std::fs::write(&state, serde_json::to_vec(&projection).unwrap()).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
        let generation =
            crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, &owner.state_root)
                .load_state_projection(session_ref)
                .unwrap()
                .run_generation;
        let original = crate::turn::PrimaryToolCall {
            tool_name: crate::native_consumer::MANAGED_CONTROL.into(),
            source_thread_id: "actual-root-thread".into(),
            request_id: 1,
            run_generation: generation,
            context: crate::primary_tool::PrimaryCallContext {
                session_id: session_ref,
                source_run_id: session_ref,
                source_turn_id: "actual-turn".into(),
                source_tool_call_id: "actual-call".into(),
                idempotency_key: "worker-key".into(),
            },
            payload: serde_json::json!({"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"context"}}),
        };
        let result = dispatch(
            &owner.state_root,
            &original,
            session_ref,
            generation,
            &owner.view.workspace_id,
        );
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["data"]["mode"], "native_only");
        let mut status = original.clone();
        status.payload = serde_json::json!({"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"request","request_id":"retained-request"}});
        status.run_generation += 1;
        let refused = dispatch(
            &owner.state_root,
            &status,
            session_ref,
            generation,
            &owner.view.workspace_id,
        );
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["action"], "status");
        assert_eq!(refused["request_id"], "retained-request");
        assert_eq!(refused["error"]["code"], "unauthorized");
        assert_eq!(refused["error"]["effect"], "not_called");
        for axis in ["generation", "session", "source", "thread", "tool"] {
            let mut call = original.clone();
            match axis {
                "generation" => call.run_generation += 1,
                "session" => call.context.session_id = Uuid::now_v7(),
                "source" => call.context.source_run_id = Uuid::now_v7(),
                "thread" => call.source_thread_id.clear(),
                _ => call.tool_name = "unregistered".into(),
            }
            let result = dispatch(
                &owner.state_root,
                &call,
                session_ref,
                generation,
                &owner.view.workspace_id,
            );
            assert!(
                crate::consumer_bridge::native_failed(&result),
                "{axis}/{result}"
            );
        }
    }
}
