//! Selected facade projection over authenticated native or Worker-owned calls.
use crate::delegation_service::{
    self as service, DelegationScope, ManagedCallContext, RunSettlementPort, SettlementOwner,
};
use crate::host_invocation::{BoundInvocation, Capability, Delivery, MethodClass};
use crate::machine::MachineError;
use crate::paths::DolgoraeHome;
use crate::trusted_host::{OwnerKind, VerifiedOwner, refusal};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub(crate) const PRIVATE: &str = "_dolgorae_host_binding";
pub(crate) const PUBLIC_LIMIT: usize = 16 * 1024 * 1024;
pub(crate) const FRAME_LIMIT: usize = PUBLIC_LIMIT + 65_536;
pub(crate) const MANAGED_TOOL: &str = "dolgorae_specialists";
pub(crate) const EXTERNAL: &[&str] = &[
    "merge_group_results",
    "assign_synthesis",
    "assign_group",
    "await_group",
    "collect_group",
    "validate_provisioning",
    "install_provisioning",
    "open_external_engagement",
    "get_external_engagement",
    "close_external_engagement",
    "hire_external_specialist",
    "assign_external_specialist_task",
    "await_external_specialist_task",
    "collect_external_specialist_task",
    "cancel_external_specialist_task",
    "release_external_specialist",
    "get_external_delegation_identity",
    "lookup_operation",
];
pub(crate) const MANAGED: &[&str] = &[
    "merge_group_results",
    "assign_synthesis",
    "assign_group",
    "await_group",
    "collect_group",
    "validate_provisioning",
    "install_provisioning",
    "lookup_operation",
    "spawn_specialist",
    "assign_specialist_task",
    "await_specialist_task",
    "collect_specialist_task",
    "cancel_specialist_task",
    "release_specialist",
    "get_managed_delegation_identity",
];
fn typed<T: DeserializeOwned>(value: &Value) -> Result<T, MachineError> {
    serde_json::from_value(value.clone()).map_err(|_| refusal())
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, MachineError> {
    value[key].as_str().ok_or_else(refusal)
}
fn id(value: &Value, key: &str) -> Result<Uuid, MachineError> {
    let text = text(value, key)?;
    let id = Uuid::try_parse(text).map_err(|_| {
        MachineError::invalid_argument(key, "reference is not a canonical retained UUIDv7")
    })?;
    if id.to_string() != text
        || id.get_version_num() != 7
        || id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(MachineError::invalid_argument(
            key,
            "reference is not a canonical retained UUIDv7",
        ));
    }
    Ok(id)
}
fn revision(value: &Value) -> Result<Option<u64>, MachineError> {
    typed(&value["expected_revision"])
}
fn response(operation: &str) -> Value {
    json!({"schema":"dolgorae.external-specialist-response/v5","operation":operation})
}

pub(crate) fn validates(value: &Value, definition: &str) -> bool {
    let schemas = crate::host_invocation::request_schemas();
    schemas[0]["$defs"]
        .get(definition)
        .is_some_and(|rule| crate::host_invocation::schema_matches(value, rule, &schemas[0], 0))
}
pub(crate) fn failure(operation: &str, error: &MachineError, entered: bool) -> Value {
    let schemas = crate::host_invocation::request_schemas();
    let mut detail = error.details.clone();
    if !crate::host_invocation::schema_matches(
        &detail,
        &schemas[1]["$defs"]["error"],
        &schemas[1],
        0,
    ) {
        let read = crate::host_invocation::method_class(operation) == Ok(MethodClass::Read);
        let unknown = entered && !read;
        let recovery = error.code == "RECOVERY_REQUIRED";
        let code = if error.code.len() <= 128
            && error
                .code
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_uppercase)
            && error
                .code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            error.code.as_str()
        } else {
            "OUTCOME_UNKNOWN"
        };
        detail = json!({"schema":"dolgorae.successor-error/v1","code":code,"operation_kind":operation,"operation_ref":null,"effect":if unknown{"unresolved"}else{"none"},"classification":if unknown{"Unknown"}else if recovery{"RecoveryNeeded"}else{"Rejected"},"required_action":if unknown{"reconcile_operation"}else{"restore_state"},"retry_new_key":false});
    }
    detail["operation_kind"] = json!(operation);
    json!({"schema":"dolgorae.external-specialist-failure/v5","operation":operation,"error":detail})
}

/// Produce a self-contained model schema from the reachable frozen definitions.
fn input_schema_for(operations: &[&str], preparation: bool) -> Value {
    use std::collections::BTreeMap;
    let schemas = crate::host_invocation::request_schemas();
    let mut definitions = BTreeMap::<String, Value>::new();
    fn local(value: &mut Value, schemas: &[Value], definitions: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object
                    .get("$ref")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    let (uri, pointer) = reference.split_once('#').expect("checked reference");
                    let index = schemas
                        .iter()
                        .position(|s| s["$id"] == uri)
                        .expect("selected schema closure");
                    let name = format!(
                        "s{index}_{}",
                        pointer.strip_prefix("/$defs/").expect("checked definition")
                    );
                    object.insert("$ref".into(), json!(format!("#/$defs/{name}")));
                    if !definitions.contains_key(&name) {
                        let mut target = schemas[index]
                            .pointer(pointer)
                            .expect("checked target")
                            .clone();
                        definitions.insert(name.clone(), Value::Null);
                        local(&mut target, schemas, definitions);
                        definitions.insert(name, target);
                    }
                }
                for (key, child) in object {
                    if key != "$ref" {
                        local(child, schemas, definitions);
                    }
                }
            }
            Value::Array(items) => {
                for child in items {
                    local(child, schemas, definitions)
                }
            }
            _ => {}
        }
    }
    let mut choices=operations.iter().map(|op|json!({"$ref":format!("{}#/$defs/{op}_request",schemas[0]["$id"].as_str().expect("schema ID"))})).collect::<Vec<_>>();
    if preparation {
        for operation in ["prepare_provisioning", "prepare_assignment"] {
            choices.push(json!({"$ref":format!("{}#/$defs/{operation}_request",schemas[4]["$id"].as_str().expect("schema ID"))}));
        }
    }
    for choice in &mut choices {
        local(choice, schemas, &mut definitions);
    }
    let mut operations = operations.to_vec();
    if preparation {
        operations.extend(["prepare_provisioning", "prepare_assignment"]);
    }
    let schemas = if preparation {
        json!({"enum":["dolgorae.external-specialist-request/v5",crate::native_preparation::SCHEMA]})
    } else {
        json!({"const":"dolgorae.external-specialist-request/v5"})
    };
    json!({"type":"object","properties":{"schema":schemas,"operation":{"enum":operations}},"required":["schema","operation"],"dependentSchemas":{"operation":{"oneOf":choices}},"$defs":definitions})
}

pub(crate) fn tool_spec(name: &str, operations: &[&str]) -> Value {
    json!({"name":name,"description":"Use checked Specialist operations under the configured authority.","inputSchema":input_schema_for(operations, name == crate::native_consumer::MANAGED_NATIVE)})
}

pub(crate) enum NativeCarrier<'a> {
    Codex { session: &'a str, call: &'a str },
    Claude(&'a Capability),
}

fn native_binding(
    home: &DolgoraeHome,
    path: &Path,
    tool: &str,
    carrier: NativeCarrier<'_>,
    request: &Value,
) -> Result<(VerifiedOwner, BoundInvocation), MachineError> {
    let owner = VerifiedOwner::load(home, path)?;
    if owner.configuration.owner != OwnerKind::External {
        return Err(refusal());
    }
    let bound = {
        let host = VerifiedOwner::load(home, path)?.ready()?;
        let bound = match carrier {
            NativeCarrier::Codex { session, call } => {
                host.codex_delivery(session, call, request)?
            }
            NativeCarrier::Claude(cap) => host.claude_carrier_delivery(cap, request)?,
        };
        if bound.context().tool_binding != tool {
            return Err(refusal());
        }
        bound
    };
    bound.require_owner(&owner)?;
    Ok((owner, bound))
}
fn external_bridge<'a>(
    owner: &'a VerifiedOwner,
    bound: &BoundInvocation,
    port: &'a dyn RunSettlementPort,
) -> Result<crate::consumer_bridge::Bridge<'a>, MachineError> {
    let witness = crate::native_consumer::Witness::external(owner, bound)?;
    Ok(crate::consumer_bridge::Bridge {
        store: crate::native_consumer::Store::open(owner, witness)?,
        source: crate::consumer_bridge::ProductSource {
            owner,
            port,
            managed: None,
        },
    })
}
pub(crate) fn control_failure(request: &Value, error: &MachineError) -> Value {
    let correlation = crate::native_consumer::encode(request)
        .and_then(|bytes| crate::native_consumer::Control::parse(&bytes))
        .ok()
        .map(|control| control.correlation());
    control_failure_for(correlation.as_ref(), error)
}
pub(crate) fn control_failure_for(
    correlation: Option<&crate::native_consumer::ControlCorrelation>,
    error: &MachineError,
) -> Value {
    let details = match error.code.as_str() {
        "CONSUMER_CONTROL_REFUSED" => error.details.clone(),
        "IDEMPOTENCY_CONFLICT" => {
            json!({"code":"request_conflict","phase":"admission","effect":"not_called","recovery":"fix_request"})
        }
        "RUN_BUSY" => {
            json!({"code":"busy","phase":"admission","effect":"not_called","recovery":"fix_request"})
        }
        _ => {
            json!({"code":"unauthorized","phase":"authority","effect":"not_called","recovery":"fix_request"})
        }
    };
    json!({"schema":"dolgorae.consumer-control-response/v1","action":correlation.map_or("unknown",|c|c.action),"ok":false,
        "request_id":correlation.and_then(|c|c.request_id.as_deref()),"error":details})
}
fn external_control(
    home: &DolgoraeHome,
    path: &Path,
    tool: &str,
    carrier: NativeCarrier<'_>,
    request: &Value,
    port: &dyn RunSettlementPort,
) -> Value {
    let result = (|| {
        let (owner, bound) = native_binding(home, path, tool, carrier, request)?;
        let control =
            crate::native_consumer::Control::parse(&crate::native_consumer::encode(request)?)?;
        let bridge = external_bridge(&owner, &bound, port)?;
        Ok(bridge.handle(&mut crate::podway_client::PublicCli, &control))
    })();
    result.unwrap_or_else(|e| control_failure(request, &e))
}

/// The host lock ends before the first product operation or observation wait.
pub(crate) fn external_with_port(
    home: &DolgoraeHome,
    path: &Path,
    tool: &str,
    carrier: NativeCarrier<'_>,
    request: &Value,
    port: &dyn RunSettlementPort,
) -> Value {
    if tool == crate::native_consumer::EXTERNAL_CONTROL {
        return external_control(home, path, tool, carrier, request, port);
    }
    let native = tool == crate::native_consumer::EXTERNAL_NATIVE;
    let mut native_context = None;
    let mut post_error = None;
    let operation = request["operation"].as_str().unwrap_or("lookup_operation");
    let mut entered = false;
    let result = (|| {
        crate::host_invocation::validate_public_request(request)?;
        if !EXTERNAL.contains(&operation) {
            return Err(refusal());
        }
        let owner = VerifiedOwner::load(home, path)?;
        if owner.configuration.owner != OwnerKind::External {
            return Err(refusal());
        }
        let binding = {
            let store = VerifiedOwner::load(home, path)?.ready()?;
            let binding = match carrier {
                NativeCarrier::Codex { session, call } => {
                    store.codex_delivery(session, call, request)?
                }
                NativeCarrier::Claude(cap) => store.claude_carrier_delivery(cap, request)?,
            };
            if binding.context().tool_binding != tool {
                return Err(refusal());
            }
            binding
        };
        owner.revalidate()?;
        let trusted = if crate::host_invocation::method_class(operation)? == MethodClass::Keyed {
            Some(binding.trusted_keyed()?)
        } else {
            None
        };
        let scope = DelegationScope {
            view: &owner.view,
            state_root: &owner.state_root,
            controller: &owner.controller,
            trusted_invocation: trusted.as_ref(),
        };
        external_preflight(&scope, request)?;
        safe_envelope(&owner, &binding, request)?;
        let decision = {
            let store = VerifiedOwner::load(home, path)?.ready()?;
            store.begin_delivery(&binding)?
        };
        owner.revalidate()?;
        entered = true;
        let result = if decision == Delivery::Reconcile {
            if trusted.is_some() {
                let lookup = {
                    let store = VerifiedOwner::load(home, path)?.ready()?;
                    store.reconcile_keyed(&binding)?
                };
                match lookup {
                    crate::engagement::IntentLookup::Accepted(receipt) => {
                        if matches!(
                            operation,
                            "assign_group" | "merge_group_results" | "assign_synthesis"
                        ) {
                            // Group replay resumes only undispatched original-key
                            // children from its retained capture, including a
                            // crash immediately after the group receipt commit.
                            dispatch(home, &owner, &scope, None, port, request, None)?
                        } else {
                            service::retained_transport_response(
                                &scope,
                                &receipt,
                                request,
                                &crate::caller_scope::TrustedCallerScope::from_port(
                                    &crate::trusted_host::ConfiguredCallerPort(&owner),
                                )?,
                            )?
                        }
                    }
                    crate::engagement::IntentLookup::NotAccepted {
                        operation_ref,
                        refusal_code,
                        ..
                    } => {
                        return Err(MachineError::new(
                            &refusal_code,
                            "original delivery was definitively refused",
                            false,
                            json!({"schema":"dolgorae.successor-error/v1","code":refusal_code,"operation_kind":operation,"operation_ref":operation_ref,"effect":"none","classification":"Rejected","required_action":"restore_state","retry_new_key":false}),
                        ));
                    }
                    crate::engagement::IntentLookup::Unresolved { operation_ref, .. } => {
                        return Err(MachineError::new(
                            "OUTCOME_UNKNOWN",
                            "original keyed delivery requires lookup",
                            false,
                            json!({"schema":"dolgorae.successor-error/v1","code":"OUTCOME_UNKNOWN","operation_kind":operation,"operation_ref":operation_ref,"effect":"unresolved","classification":"Unknown","required_action":"reconcile_operation","retry_new_key":false}),
                        ));
                    }
                    crate::engagement::IntentLookup::Absent { .. } => {
                        return Err(MachineError::new(
                            "OUTCOME_UNKNOWN",
                            "delivery marker has no product acknowledgement",
                            false,
                            json!({}),
                        ));
                    }
                }
            } else {
                let target = external_owner(request)?;
                service::observe_state_control(
                    &scope,
                    target,
                    None,
                    operation_static(operation)?,
                    optional_id(request, "member_ref")?,
                    optional_id(request, "task_ref")?,
                    state_body(request).as_ref(),
                )?
            }
        } else {
            let admission = || {
                external_bridge(&owner, &binding, port)?
                    .assignment_binding(&mut crate::podway_client::PublicCli, request)
            };
            dispatch(
                home,
                &owner,
                &scope,
                None,
                port,
                request,
                native.then_some(&admission),
            )?
        };
        if trusted.is_some() {
            let store = VerifiedOwner::load(home, path)?.ready()?;
            store.reconcile_keyed(&binding)?;
        }
        if native {
            post_error = owner.revalidate().err();
            native_context = Some((owner, binding));
        } else {
            owner.revalidate()?;
        }
        Ok(result)
    })();
    let public = finish(operation, result, entered);
    if !native {
        return public;
    }
    let mut presented = match native_context {
        Some((owner, binding))
            if public["schema"] != "dolgorae.external-specialist-failure/v5"
                && (public.get("identity").is_some()
                    || public["task"]["complete_result"].is_object()) =>
        {
            match external_bridge(&owner, &binding, port) {
                Ok(bridge) => {
                    bridge.semantic(&mut crate::podway_client::PublicCli, request, &public)
                }
                Err(e) => {
                    post_error = Some(e);
                    crate::consumer_bridge::semantic_presentation(&public)
                }
            }
        }
        _ => crate::consumer_bridge::semantic_presentation(&public),
    };
    if let Some(e) = post_error {
        presented["ok"] = json!(false);
        presented["presentation_error"] =
            json!({"code":e.code,"recovery":"inspect original operation; do not redispatch"});
    }
    presented
}
pub(crate) fn finish(operation: &str, result: Result<Value, MachineError>, entered: bool) -> Value {
    match result {
        Ok(value) if validates(&value, &format!("{operation}_response")) => value,
        Ok(_) => failure(operation, &refusal(), entered),
        Err(error) => failure(operation, &error, entered),
    }
}
fn operation_static(op: &str) -> Result<&'static str, MachineError> {
    EXTERNAL
        .iter()
        .chain(MANAGED)
        .copied()
        .find(|s| *s == op)
        .ok_or_else(refusal)
}
fn state_body(request: &Value) -> Option<Value> {
    match request["operation"].as_str() {
        Some("cancel_external_specialist_task" | "cancel_specialist_task") => Some(
            json!({"member_ref":request["member_ref"],"task_ref":request["task_ref"],"reason":request["reason"]}),
        ),
        Some("close_external_engagement") => {
            Some(json!({"allow_interrupt":request["allow_interrupt"]}))
        }
        _ => None,
    }
}
fn optional_id(value: &Value, key: &str) -> Result<Option<Uuid>, MachineError> {
    value.get(key).map(|_| id(value, key)).transpose()
}
fn external_owner(request: &Value) -> Result<SettlementOwner, MachineError> {
    Ok(SettlementOwner::External {
        engagement_id: if request.get("engagement_ref").is_some() {
            id(request, "engagement_ref")?
        } else {
            id(request, "scope_ref")?
        },
    })
}
fn external_scope(request: &Value) -> Option<(&str, &str)> {
    if request["operation"] == "lookup_operation" {
        request["namespace"]["scope_kind"]
            .as_str()
            .zip(request["namespace"]["scope_ref"].as_str())
    } else if let Some(reference) = request["engagement_ref"].as_str() {
        Some(("external_engagement", reference))
    } else {
        request["scope_kind"]
            .as_str()
            .zip(request["scope_ref"].as_str())
    }
}
pub(crate) fn typed_preflight(request: &Value) -> Result<(), MachineError> {
    for field in ["member_ref", "task_ref", "engagement_ref"] {
        if request.get(field).is_some() {
            id(request, field)?;
        }
    }
    Ok(())
}
fn external_preflight(scope: &DelegationScope<'_>, request: &Value) -> Result<(), MachineError> {
    typed_preflight(request)?;
    if let Some((kind, reference)) = external_scope(request) {
        if kind != "external_engagement" {
            return Err(refusal());
        }
        let engagement_id = id(&json!({"scope_ref":reference}), "scope_ref")?;
        service::transport_owner(
            scope,
            SettlementOwner::External { engagement_id },
            None,
            operation_static(text(request, "operation")?)?,
        )?;
    }
    Ok(())
}

fn safe_envelope(
    owner: &VerifiedOwner,
    binding: &BoundInvocation,
    request: &Value,
) -> Result<(), MachineError> {
    let scope = external_scope(request).unwrap_or(("workspace", owner.view.workspace_id.as_str()));
    let ns = crate::operation::Namespace::delegation(
        &owner.controller.identity,
        &owner.view.workspace_id,
        None,
        "open_external_engagement",
        "safe-envelope",
    )
    .map_err(|_| refusal())?;
    let envelope = json!({"schema":"dolgorae.trusted-specialist-transport/v1","request":request,"trusted_invocation":binding.context(),"controller_principal":ns.controller_principal,"controller_generation":owner.controller.identity.generation,"authorized_scope_kind":scope.0,"authorized_scope_ref":scope.1,"caller_scope":owner.configuration.caller_scope});
    if !validates(&envelope, "trusted_transport_envelope") {
        return Err(refusal());
    }
    Ok(())
}

fn checked_identity(identity: Value) -> Result<Value, MachineError> {
    if identity["worker_disposition"] != "released" {
        return Err(MachineError::new(
            "RECOVERY_REQUIRED",
            "consistent active disposition is not available",
            false,
            json!({}),
        ));
    }
    Ok(identity)
}

pub(crate) fn dispatch(
    home: &DolgoraeHome,
    owner: &VerifiedOwner,
    scope: &DelegationScope<'_>,
    managed: Option<&ManagedCallContext<'_>>,
    port: &dyn RunSettlementPort,
    request: &Value,
    assignment_admission: Option<&dyn Fn() -> Result<(), MachineError>>,
) -> Result<Value, MachineError> {
    let operation = text(request, "operation")?;
    let mut result = response(operation);
    let caller = owner
        .configuration
        .caller_scope
        .as_ref()
        .map(|_| {
            crate::caller_scope::TrustedCallerScope::from_port(
                &crate::trusted_host::ConfiguredCallerPort(owner),
            )
        })
        .transpose()?;
    let key = request["idempotency_key"].as_str().unwrap_or("");
    match operation {
        "assign_synthesis" => {
            return service::research_merge::assign(
                scope,
                managed,
                caller.as_ref(),
                port,
                request,
                false,
            );
        }
        "merge_group_results" => {
            return service::research_merge::merge(scope, managed, port, request, false);
        }
        "assign_group" => {
            return service::research_group::assign(
                scope,
                managed,
                crate::caller_scope::TrustedCallerScope::require(caller.as_ref())?,
                port,
                request,
            );
        }
        "collect_group" => {
            return service::research_group::collect(scope, managed, port, request);
        }
        "await_group" => {
            return service::research_group::await_group(scope, managed, port, request);
        }
        "validate_provisioning" => {
            let expected = typed(&request["sources"])?;
            result["candidate"] = serde_json::to_value(
                crate::source_binding::fresh_provisioning(home, owner, &expected)?
                    .candidate(false)?,
            )
            .map_err(|_| refusal())?;
        }
        "install_provisioning" => {
            let expected = typed(&request["sources"])?;
            let (candidate, receipt) = service::install_bound_provisioning(
                scope,
                &expected,
                key,
                revision(request)?,
                || crate::source_binding::fresh_provisioning(home, owner, &expected),
            )?;
            result["candidate"] = serde_json::to_value(candidate).map_err(|_| refusal())?;
            result["receipt"] = json!(receipt);
        }
        "open_external_engagement" => {
            let (engagement, receipt) =
                service::open_external_engagement(scope, key, revision(request)?)?;
            result["engagement"] = json!(engagement);
            result["receipt"] = json!(receipt);
        }
        "get_external_engagement" => {
            result["engagement"] = json!(service::get_external_engagement(
                scope,
                id(request, "engagement_ref")?
            )?)
        }
        "close_external_engagement" => {
            return service::close_external_engagement(
                scope,
                port,
                id(request, "engagement_ref")?,
                request["expected_revision"].as_u64().ok_or_else(refusal)?,
                request["allow_interrupt"].as_bool().ok_or_else(refusal)?,
            );
        }
        "hire_external_specialist" | "spawn_specialist" => {
            let provisioning_ref = text(request, "provisioning_ref")?;
            let role_ref = text(request, "role_ref")?;
            let agent_configuration_digest = text(request, "agent_configuration_digest")?;
            let admitted = if let Some(context) = managed {
                let request = service::SpawnSpecialist {
                    provisioning_ref,
                    role_ref,
                    agent_configuration_digest,
                    expected_revision: revision(request)?,
                };
                if owner.configuration.caller_scope.is_none() {
                    service::spawn_primary(scope, context, port, request, owner)?
                } else {
                    service::spawn_specialist(scope, context, port, request)?
                }
            } else {
                service::hire_external_specialist(
                    scope,
                    port,
                    service::HireExternalSpecialist {
                        engagement_id: id(request, "scope_ref")?,
                        provisioning_ref,
                        role_ref,
                        agent_configuration_digest,
                        idempotency_key: key,
                        expected_revision: revision(request)?,
                    },
                )?
            };
            result["member"] = json!(admitted.member);
            result["receipt"] = json!(admitted.receipt);
        }
        "assign_external_specialist_task" | "assign_specialist_task" => {
            let task: crate::task_request::SpecialistTaskRequest =
                typed(&request["assignment"]["task_body"])?;
            let expected: crate::delegation_identity::AcceptedTask =
                typed(&request["assignment"]["accepted_task_identity"])?;
            let execution_intent = expected.execution_intent.clone();
            let member = id(request, "member_ref")?;
            let deadline = request["assignment"]["deadline_seconds"]
                .as_u64()
                .ok_or_else(refusal)?;
            let ns = crate::operation::Namespace::delegation(
                &owner.controller.identity,
                &owner.view.workspace_id,
                Some((text(request, "scope_kind")?, text(request, "scope_ref")?)),
                operation,
                key,
            )
            .map_err(|_| refusal())?;
            let source = crate::source_binding::RegisteredAssignment {
                owner,
                supplied: crate::source_binding::AssignmentSelection {
                    namespace: (&ns).into(),
                    member_ref: member,
                    task: task.clone(),
                    execution_intent: execution_intent.clone(),
                    deadline_seconds: deadline,
                    target: None,
                },
                expected,
                admission: assignment_admission,
            };
            let (observation, receipt) = if let Some(context) = managed {
                if let Some(caller) = &caller {
                    service::assign_managed_with_source(
                        scope,
                        context,
                        caller,
                        port,
                        service::AssignSpecialistTask {
                            member_ref: member,
                            task,
                            execution_intent: &execution_intent,
                            target: None,
                            deadline_seconds: deadline,
                            expected_revision: revision(request)?,
                        },
                        &source,
                    )?
                } else {
                    service::assign_primary_with_source(
                        scope,
                        context,
                        &crate::native_preparation::Origin::from_owner(owner)?,
                        port,
                        service::AssignSpecialistTask {
                            member_ref: member,
                            task,
                            execution_intent: &execution_intent,
                            target: None,
                            deadline_seconds: deadline,
                            expected_revision: revision(request)?,
                        },
                        &source,
                    )?
                }
            } else {
                service::assign_external_with_source(
                    scope,
                    crate::caller_scope::TrustedCallerScope::require(caller.as_ref())?,
                    port,
                    service::AssignExternalSpecialistTask {
                        engagement_id: id(request, "scope_ref")?,
                        member_ref: member,
                        task,
                        execution_intent: &execution_intent,
                        target: None,
                        deadline_seconds: deadline,
                        idempotency_key: key,
                        expected_revision: revision(request)?,
                    },
                    &source,
                )?
            };
            result["task"] = json!(observation);
            result["receipt"] = json!(receipt);
        }
        "await_external_specialist_task" | "await_specialist_task" => {
            let member = id(request, "member_ref")?;
            let tasks = [id(request, "task_ref")?];
            let timeout = request["timeout_seconds"].as_u64().ok_or_else(refusal)?;
            let (tasks, timed_out) = if let Some(context) = managed {
                service::await_specialist_tasks(
                    scope,
                    context,
                    service::AwaitSpecialistTasks {
                        member_ref: member,
                        tasks: &tasks,
                        predicate: service::AwaitPredicate::All,
                        timeout_seconds: timeout,
                    },
                )?
            } else {
                service::await_external_specialist_tasks(
                    scope,
                    service::AwaitExternalSpecialistTasks {
                        engagement_id: id(request, "scope_ref")?,
                        member_ref: member,
                        tasks: &tasks,
                        predicate: service::AwaitPredicate::All,
                        timeout_seconds: timeout,
                    },
                )?
            };
            result["task"] = json!(tasks.into_iter().next().ok_or_else(refusal)?);
            result["wait_timed_out"] = json!(timed_out);
        }
        "collect_external_specialist_task" | "collect_specialist_task" => {
            let member = id(request, "member_ref")?;
            let task = id(request, "task_ref")?;
            let collected = if let Some(context) = managed {
                service::collect_specialist_task(
                    scope,
                    context,
                    port,
                    service::CollectSpecialistTask {
                        member_ref: member,
                        task_ref: task,
                        adapter_id: &owner.configuration.adapter_id,
                        adapter_version: &owner.configuration.adapter_version,
                    },
                )?
            } else {
                service::collect_external_specialist_task(
                    scope,
                    port,
                    service::CollectExternalSpecialistTask {
                        engagement_id: id(request, "scope_ref")?,
                        member_ref: member,
                        task_ref: task,
                        adapter_id: &owner.configuration.adapter_id,
                        adapter_version: &owner.configuration.adapter_version,
                    },
                )?
            };
            result["task"] = json!(collected.task);
        }
        "cancel_external_specialist_task" | "cancel_specialist_task" => {
            let target = if let Some(context) = managed {
                SettlementOwner::Managed {
                    session_id: context.session_id,
                }
            } else {
                external_owner(request)?
            };
            return service::cancel_specialist_task(
                scope,
                target,
                managed,
                port,
                id(request, "member_ref")?,
                id(request, "task_ref")?,
                request["expected_revision"].as_u64().ok_or_else(refusal)?,
                text(request, "reason")?,
            );
        }
        "release_external_specialist" | "release_specialist" => {
            let member = id(request, "member_ref")?;
            let revision = request["expected_revision"].as_u64().ok_or_else(refusal)?;
            let released = if let Some(context) = managed {
                service::release_specialist(
                    scope,
                    context,
                    port,
                    service::ReleaseSpecialist {
                        member_ref: member,
                        expected_revision: revision,
                        allow_interrupt: false,
                    },
                )?
            } else {
                service::release_external_specialist(
                    scope,
                    port,
                    service::ReleaseExternalSpecialist {
                        engagement_id: id(request, "scope_ref")?,
                        member_ref: member,
                        expected_revision: revision,
                        allow_interrupt: false,
                    },
                )?
            };
            let target = if let Some(context) = managed {
                SettlementOwner::Managed {
                    session_id: context.session_id,
                }
            } else {
                external_owner(request)?
            };
            let mut member = service::transport_member(scope, target, member, managed)?;
            member.state = released.state.into();
            member.revision = released.revision;
            result["member"] = json!(member);
            result["operation_ref"] = json!(released.operation.operation_ref);
        }
        "get_external_delegation_identity" | "get_managed_delegation_identity" => {
            let member = id(request, "member_ref")?;
            let task = id(request, "task_ref")?;
            let view = if let Some(context) = managed {
                service::get_managed_delegation_identity(
                    scope,
                    context,
                    crate::caller_scope::TrustedCallerScope::require(caller.as_ref())?,
                    port,
                    service::CollectSpecialistTask {
                        member_ref: member,
                        task_ref: task,
                        adapter_id: &owner.configuration.adapter_id,
                        adapter_version: &owner.configuration.adapter_version,
                    },
                )?
            } else {
                service::get_external_delegation_identity(
                    scope,
                    crate::caller_scope::TrustedCallerScope::require(caller.as_ref())?,
                    port,
                    service::CollectExternalSpecialistTask {
                        engagement_id: id(request, "scope_ref")?,
                        member_ref: member,
                        task_ref: task,
                        adapter_id: &owner.configuration.adapter_id,
                        adapter_version: &owner.configuration.adapter_version,
                    },
                )?
            };
            let identity = serde_json::to_value(view.identity).map_err(|_| refusal())?;
            result["identity"] = checked_identity(identity)?;
        }
        "lookup_operation" => {
            let ns: crate::delegation_identity::LookupNamespace = typed(&request["namespace"])?;
            result["lookup"] = service::lookup_operation(
                scope,
                &ns.operation_kind,
                ns.scope_kind.as_deref().zip(ns.scope_ref.as_deref()),
                &ns.idempotency_key,
            )?;
        }
        _ => return Err(refusal()),
    }
    Ok(result)
}

pub(crate) fn external(
    home: &DolgoraeHome,
    path: &Path,
    tool: &str,
    carrier: NativeCarrier<'_>,
    request: &Value,
) -> Value {
    let port = (|| {
        let owner = VerifiedOwner::load(home, path)?;
        crate::controller::CredentialCarrier::open_path(&owner.configuration.controller_file)
            .map_err(|_| refusal())
    })();
    match port {
        Ok(carrier_file) => external_with_port(
            home,
            path,
            tool,
            carrier,
            request,
            &crate::semantic::SemanticRunSettlementPort {
                carrier: &carrier_file,
            },
        ),
        Err(error) if crate::native_consumer::is_control(tool) => control_failure(request, &error),
        Err(error) => {
            let public = failure(
                request["operation"].as_str().unwrap_or("lookup_operation"),
                &error,
                false,
            );
            if tool == crate::native_consumer::EXTERNAL_NATIVE {
                crate::consumer_bridge::semantic_presentation(&public)
            } else {
                public
            }
        }
    }
}

/// Private original-call observation. Neither K nor S dispatches an effect.
pub(crate) fn lookup_native(
    home: &DolgoraeHome,
    path: &Path,
    session: &str,
    call: &str,
) -> Result<Value, MachineError> {
    let owner = VerifiedOwner::load(home, path)?;
    if owner.configuration.owner != OwnerKind::External {
        return Err(refusal());
    }
    let binding = {
        let store = VerifiedOwner::load(home, path)?.ready()?;
        store.native_lookup(session, call)?
    };
    let request = binding.request();
    if binding.context().tool_binding == crate::native_consumer::EXTERNAL_CONTROL {
        let mut control =
            crate::native_consumer::Control::parse(&crate::native_consumer::encode(request)?)?;
        if let Some(id) = control.request_id().map(str::to_owned) {
            control.action = crate::native_consumer::Action::Status {
                target: crate::native_consumer::StatusTarget::Request { request_id: id },
            };
        }
        let carrier =
            crate::controller::CredentialCarrier::open_path(&owner.configuration.controller_file)
                .map_err(|_| refusal())?;
        let port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
        return Ok(external_bridge(&owner, &binding, &port)?
            .handle(&mut crate::podway_client::PublicCli, &control));
    }
    let operation = text(request, "operation")?;
    let scope = DelegationScope {
        view: &owner.view,
        state_root: &owner.state_root,
        controller: &owner.controller,
        trusted_invocation: None,
    };
    external_preflight(&scope, request)?;
    let result = match crate::host_invocation::method_class(operation)? {
        MethodClass::Keyed => {
            let lookup = {
                let store = VerifiedOwner::load(home, path)?.ready()?;
                store.reconcile_keyed(&binding)?
            };
            let _ = lookup;
            // The service owns the checked lookup projection and its classifications.
            let ns = binding.namespace()?.clone();
            json!({"schema":"dolgorae.external-specialist-response/v5","operation":"lookup_operation","lookup":service::lookup_operation(&scope,&ns.operation_kind,ns.scope_kind.as_deref().zip(ns.scope_ref.as_deref()),&ns.idempotency_key)?})
        }
        MethodClass::Read => {
            let carrier = crate::controller::CredentialCarrier::open_path(
                &owner.configuration.controller_file,
            )
            .map_err(|_| refusal())?;
            dispatch(
                home,
                &owner,
                &scope,
                None,
                &crate::semantic::SemanticRunSettlementPort { carrier: &carrier },
                request,
                None,
            )?
        }
        MethodClass::State => service::observe_state_control(
            &scope,
            external_owner(request)?,
            None,
            operation_static(operation)?,
            optional_id(request, "member_ref")?,
            optional_id(request, "task_ref")?,
            state_body(request).as_ref(),
        )?,
    };
    owner.revalidate()?;
    let public = finish(
        result["operation"].as_str().ok_or_else(refusal)?,
        Ok(result.clone()),
        true,
    );
    if binding.context().tool_binding == crate::native_consumer::EXTERNAL_NATIVE {
        let carrier =
            crate::controller::CredentialCarrier::open_path(&owner.configuration.controller_file)
                .map_err(|_| refusal())?;
        let port = crate::semantic::SemanticRunSettlementPort { carrier: &carrier };
        if public.get("identity").is_some() || public["task"]["complete_result"].is_object() {
            Ok(external_bridge(&owner, &binding, &port)?.semantic(
                &mut crate::podway_client::PublicCli,
                request,
                &public,
            ))
        } else {
            Ok(crate::consumer_bridge::semantic_presentation(&public))
        }
    } else {
        Ok(public)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_control_authority_failure_keeps_status_request_correlation() {
        let fixture =
            crate::trusted_host::tests::Fixture::new(crate::trusted_host::HostRoute::Codex);
        fixture.complete();
        let request = json!({"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"request","request_id":"original-request"}});
        let result = external_with_port(
            &fixture.home,
            &fixture.path(),
            crate::native_consumer::EXTERNAL_CONTROL,
            NativeCarrier::Codex {
                session: "unbound-session",
                call: "unbound-call",
            },
            &request,
            &crate::delegation_service::tests::FakePort::new(),
        );
        assert_eq!(result["ok"], false);
        assert_eq!(result["action"], "status");
        assert_eq!(result["request_id"], "original-request");
        assert_eq!(result["error"]["code"], "unauthorized");
        assert_eq!(result["error"]["effect"], "not_called");
    }
    #[test]
    fn frozen_response_reference_closure_preserves_hash_and_unicode_path_domains() {
        let schemas = crate::host_invocation::request_schemas();
        let rule = json!({"$ref":"https://dolgorae.local/schema/review-target/v2#/$defs/safe_relative_path"});
        for path in [
            "file".to_owned(),
            "a/./b".into(),
            "a//b".into(),
            "é".repeat(8192),
        ] {
            assert!(crate::host_invocation::schema_matches(
                &json!(path),
                &rule,
                &schemas[0],
                0
            ));
        }
        for path in [
            "/absolute".to_owned(),
            "../escape".into(),
            "a/../b".into(),
            "line\nbreak".into(),
            "a\u{2028}b".into(),
            "é".repeat(8193),
        ] {
            assert!(!crate::host_invocation::schema_matches(
                &json!(path),
                &rule,
                &schemas[0],
                0
            ));
        }
        let rule = json!({"type":"string","pattern":"^[0-9a-f]{64}$"});
        for length in 0..=65 {
            assert_eq!(
                crate::host_invocation::schema_matches(
                    &json!("a".repeat(length)),
                    &rule,
                    &schemas[0],
                    0
                ),
                length == 64
            );
            assert!(!crate::host_invocation::schema_matches(
                &json!("A".repeat(length)),
                &rule,
                &schemas[0],
                0
            ));
        }
        let uuid_rule = &schemas[1]["$defs"]["uuid7"];
        for (text, expected) in [
            ("018f1111-2222-7333-8444-555555555555", true),
            ("018F1111-2222-7333-8444-555555555555", false),
            ("018f1111222273338444555555555555", false),
            ("{018f1111-2222-7333-8444-555555555555}", false),
            ("urn:uuid:018f1111-2222-7333-8444-555555555555", false),
            ("018f1111-2222-4333-8444-555555555555", false),
            ("018f1111-2222-7333-c444-555555555555", false),
        ] {
            assert_eq!(
                crate::host_invocation::schema_matches(&json!(text), uuid_rule, &schemas[1], 0),
                expected,
                "{text}"
            );
        }
        let code_rule = &schemas[1]["$defs"]["safe_code"];
        for (text, expected) in [
            ("A", true),
            ("A0_B", true),
            ("a", false),
            ("0A", false),
            ("A-", false),
            ("A\nB", false),
            ("É", false),
            ("", false),
        ] {
            assert_eq!(
                crate::host_invocation::schema_matches(&json!(text), code_rule, &schemas[1], 0),
                expected,
                "{text:?}"
            );
        }
        let complete: Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-complete-result.valid.json"
        ))
        .unwrap();
        assert!(crate::host_invocation::schema_matches(
            &complete,
            &json!({"$ref":"https://dolgorae.local/schema/v014-identities/v1#/$defs/complete_result"}),
            &schemas[0],
            0
        ));
    }
    #[test]
    fn selected_facade_contract_examples_and_required_fields_match_the_shared_validator() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/successor-facade-v5-methods.valid.json"
        ))
        .unwrap();
        let mut selected = 0;
        for case in fixtures["cases"].as_array().unwrap() {
            let operation = case["operation"].as_str().unwrap();
            if !EXTERNAL.contains(&operation) && !MANAGED.contains(&operation) {
                continue;
            }
            selected += 1;
            for field in ["request", "response", "failure"] {
                let definition = if field == "failure" {
                    "failure".into()
                } else {
                    format!("{operation}_{field}")
                };
                let value = &case[field];
                assert!(validates(value, &definition), "{operation}/{field}");
                let rule = &crate::host_invocation::request_schemas()[0]["$defs"][&definition];
                for required in rule["required"].as_array().unwrap() {
                    let mut missing = value.clone();
                    missing
                        .as_object_mut()
                        .unwrap()
                        .remove(required.as_str().unwrap());
                    assert!(
                        !validates(&missing, &definition),
                        "missing {operation}/{field}/{required}"
                    );
                }
                let mut extra = value.clone();
                extra["controller_file"] = json!("/private/model-secret");
                assert!(!validates(&extra, &definition));
            }
        }
        assert_eq!(selected, 25);
    }
    #[test]
    fn external_actual_scope_and_typed_references_precede_delivery_markers() {
        use crate::trusted_host::{HostRoute, tests::Fixture};
        let f = Fixture::new(HostRoute::Codex);
        f.complete();
        let foreign = Fixture::new(HostRoute::Codex);
        foreign.complete();
        let owner = f.owner();
        let other = foreign.owner();
        let scope = DelegationScope {
            view: &owner.view,
            state_root: &owner.state_root,
            controller: &owner.controller,
            trusted_invocation: None,
        };
        let foreign_scope = DelegationScope {
            view: &owner.view,
            state_root: &owner.state_root,
            controller: &other.controller,
            trusted_invocation: None,
        };
        let own = service::open_external_engagement(&scope, "own", None)
            .unwrap()
            .0;
        let foreign = service::open_external_engagement(&foreign_scope, "foreign", None)
            .unwrap()
            .0;
        for (reference, accepted) in [
            (own.engagement_ref.as_str(), true),
            (foreign.engagement_ref.as_str(), false),
        ] {
            let namespace = crate::operation::Namespace::delegation(
                &owner.controller.identity,
                &owner.view.workspace_id,
                Some(("external_engagement", reference)),
                "hire_external_specialist",
                "unused",
            )
            .unwrap();
            let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"lookup_operation","namespace":crate::delegation_identity::LookupNamespace::from(&namespace)});
            let call = reference;
            {
                let store = f.ready();
                store
                    .bind_native(
                        crate::host_invocation::NativeEvent {
                            session: "native".into(),
                            turn: None,
                            tool_use: call.into(),
                            server: "dolgorae".into(),
                            server_source: None,
                            tool: "specialist".into(),
                        },
                        request.clone(),
                    )
                    .unwrap();
            }
            let result = external_with_port(
                &f.home,
                &f.path(),
                "specialist",
                NativeCarrier::Codex {
                    session: "native",
                    call,
                },
                &request,
                &crate::delegation_service::tests::FakePort::new(),
            );
            if accepted {
                assert_eq!(result["lookup"]["state"], "absent", "{result}");
            } else {
                assert_eq!(
                    result["schema"], "dolgorae.external-specialist-failure/v5",
                    "{result}"
                );
                assert_eq!(result["error"]["effect"], "none");
            }
        }
        for reference in [
            "not-a-retained-uuid",
            "018F1111-2222-7333-8444-555555555555",
        ] {
            let request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"close_external_engagement","engagement_ref":reference,"expected_revision":0,"allow_interrupt":false});
            {
                let store = f.ready();
                store
                    .bind_native(
                        crate::host_invocation::NativeEvent {
                            session: "typed".into(),
                            turn: None,
                            tool_use: reference.into(),
                            server: "dolgorae".into(),
                            server_source: None,
                            tool: "specialist".into(),
                        },
                        request.clone(),
                    )
                    .unwrap();
            }
            let result = external_with_port(
                &f.home,
                &f.path(),
                "specialist",
                NativeCarrier::Codex {
                    session: "typed",
                    call: reference,
                },
                &request,
                &crate::delegation_service::tests::FakePort::new(),
            );
            assert_eq!(result["error"]["effect"], "none", "{result}");
            let db = rusqlite::Connection::open(f.private.join("invocations.sqlite")).unwrap();
            let attempted:i64=db.query_row("SELECT attempted FROM native_calls WHERE session_ref='typed' AND tool_use_ref=?1",[reference],|row|row.get(0)).unwrap();
            assert_eq!(
                attempted, 0,
                "invalid reference crossed durable dispatch marker"
            );
        }
        for field in ["member_ref", "task_ref"] {
            for reference in [
                "not-a-retained-uuid",
                "018F1111-2222-7333-8444-555555555555",
                "018f1111-2222-4333-8444-555555555555",
                "018f1111-2222-7333-c444-555555555555",
            ] {
                let call = format!("{field}:{reference}");
                let mut request = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"cancel_external_specialist_task","scope_kind":"external_engagement","scope_ref":own.engagement_ref,"member_ref":Uuid::now_v7(),"task_ref":Uuid::now_v7(),"expected_revision":0,"reason":"invalid typed reference"});
                request[field] = json!(reference);
                {
                    let store = f.ready();
                    store
                        .bind_native(
                            crate::host_invocation::NativeEvent {
                                session: "typed-fields".into(),
                                turn: None,
                                tool_use: call.clone(),
                                server: "dolgorae".into(),
                                server_source: None,
                                tool: "specialist".into(),
                            },
                            request.clone(),
                        )
                        .unwrap();
                }
                let result = external_with_port(
                    &f.home,
                    &f.path(),
                    "specialist",
                    NativeCarrier::Codex {
                        session: "typed-fields",
                        call: &call,
                    },
                    &request,
                    &crate::delegation_service::tests::FakePort::new(),
                );
                assert_eq!(
                    result["error"]["effect"], "none",
                    "{field}/{reference}: {result}"
                );
                let db = rusqlite::Connection::open(f.private.join("invocations.sqlite")).unwrap();
                let attempted:i64=db.query_row("SELECT attempted FROM native_calls WHERE session_ref='typed-fields' AND tool_use_ref=?1",[call],|row|row.get(0)).unwrap();
                assert_eq!(attempted, 0, "invalid {field} crossed delivery marker");
            }
        }
    }

    #[test]
    fn facade_identity_projection_requires_terminal_release_and_preserves_failure_effects() {
        for (operation, bytes) in [
            (
                "get_external_delegation_identity",
                include_str!(
                    "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
                ),
            ),
            (
                "get_managed_delegation_identity",
                include_str!(
                    "../docs/protocol/examples/v014-identity-release-managed-released.valid.json"
                ),
            ),
        ] {
            let identity: Value = serde_json::from_str(bytes).unwrap();
            let mut positive = response(operation);
            positive["identity"] = checked_identity(identity.clone()).unwrap();
            assert!(validates(&positive, &format!("{operation}_response")));
            for disposition in ["idle", "running", "unknown", ""] {
                let mut changed = identity.clone();
                changed["worker_disposition"] = json!(disposition);
                let error = checked_identity(changed).unwrap_err();
                let refused = failure(operation, &error, true);
                assert_eq!(refused["error"]["effect"], "none");
                assert_eq!(refused["error"]["classification"], "RecoveryNeeded");
                assert!(validates(&refused, "failure"));
            }
        }
        let error = MachineError::new(
            "unsafe\n/private/secret",
            "/private/controller",
            false,
            json!({"credential":"secret"}),
        );
        for (operation, entered, effect) in [
            ("open_external_engagement", false, "none"),
            ("open_external_engagement", true, "unresolved"),
            ("get_external_engagement", true, "none"),
        ] {
            let value = failure(operation, &error, entered);
            assert_eq!(value["error"]["effect"], effect);
            assert!(validates(&value, "failure"));
            assert!(!value.to_string().contains("secret"));
        }
    }
}
