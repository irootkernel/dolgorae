//! Complete source reads, typed selections and explicit consumer composition.
use crate::delegation_identity::{BytesRef, DelegationIdentity};
use crate::delegation_service::{self as service, DelegationScope, RunSettlementPort};
use crate::machine::MachineError;
use crate::native_consumer::*;
use crate::podway_client::{self as podway, Client, Invocation, ReadCommand};
use crate::trusted_host::{OwnerKind, VerifiedOwner, is_digest, refusal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Selection {
    CollectedResult {
        scope: String,
        member: Uuid,
        task: Uuid,
        result_digest: String,
        accepted_task_digest: String,
    },
    Result {
        scope: String,
        member: Uuid,
        task: Uuid,
        result_digest: String,
        accepted_task_digest: String,
        projection: Value,
    },
    Context {
        preimage: Value,
    },
    Request {
        request_id: String,
    },
    Cursor {
        selection_ref: String,
        view: View,
        offset: u64,
        source: BytesRef,
    },
    Artifact {
        evidence_ref: String,
        view: View,
        source: BytesRef,
        media_type: String,
    },
}
/// Only actual authorized service getters supply the current M4 projection.
/// Content reads occur after those transactions end and are closed by a fresh
/// getter. A selection is a reference, never a cache of original result bytes.
pub(crate) struct ProductSource<'a> {
    pub owner: &'a VerifiedOwner,
    pub port: &'a dyn RunSettlementPort,
    pub managed: Option<service::ManagedCallContext<'a>>,
}
impl ProductSource<'_> {
    fn collected(
        &self,
        scope_ref: &str,
        member: Uuid,
        task: Uuid,
    ) -> Result<(crate::delegation_identity::CompleteResult, String), MachineError> {
        self.owner.revalidate()?;
        let scope = DelegationScope {
            view: &self.owner.view,
            state_root: &self.owner.state_root,
            controller: &self.owner.controller,
            trusted_invocation: None,
        };
        let id = Uuid::parse_str(scope_ref).map_err(|_| invalid())?;
        let configuration = &self.owner.configuration;
        let (collected, result) = match &configuration.owner {
            OwnerKind::External => {
                self.owner
                    .permit(Some(("external_engagement", scope_ref)))?;
                service::collect_external_specialist_task_observation(
                    &scope,
                    self.port,
                    service::CollectExternalSpecialistTask {
                        engagement_id: id,
                        member_ref: member,
                        task_ref: task,
                        adapter_id: &configuration.adapter_id,
                        adapter_version: &configuration.adapter_version,
                    },
                )?
            }
            OwnerKind::Managed { session_ref } if *session_ref == id => {
                self.owner
                    .permit(Some(("orchestrated_session", scope_ref)))?;
                service::collect_specialist_task_observation(
                    &scope,
                    self.managed.as_ref().ok_or_else(refusal)?,
                    self.port,
                    service::CollectSpecialistTask {
                        member_ref: member,
                        task_ref: task,
                        adapter_id: &configuration.adapter_id,
                        adapter_version: &configuration.adapter_version,
                    },
                )?
            }
            _ => return Err(refusal()),
        };
        let result = result.ok_or_else(source_unavailable)?;
        let digest = collected.task.result_digest.ok_or_else(source_integrity)?;
        if collected.task.accepted_task_digest != result.source.accepted_task_digest {
            return Err(source_integrity());
        }
        self.owner.revalidate()?;
        Ok((result, digest))
    }

    fn select_collected(
        &self,
        store: &Store<'_>,
        scope: &str,
        member: Uuid,
        task: Uuid,
    ) -> Result<(String, ReadMaterialized), MachineError> {
        store.authorize_source(self.owner, self.managed.as_ref())?;
        let (result, digest) = self.collected(scope, member, task)?;
        let selected = Selection::CollectedResult {
            scope: scope.into(),
            member,
            task,
            result_digest: digest,
            accepted_task_digest: result.source.accepted_task_digest.clone(),
        };
        let material = self.read_collected(scope, member, task, &result)?;
        Ok((store.retain_selection(&selected)?, material))
    }

    fn read_materialize(&self, selected: &Selection) -> Result<ReadMaterialized, MachineError> {
        let (scope, member, task, digest, accepted) = match selected {
            Selection::CollectedResult {
                scope,
                member,
                task,
                result_digest,
                accepted_task_digest,
            }
            | Selection::Result {
                scope,
                member,
                task,
                result_digest,
                accepted_task_digest,
                ..
            } => (scope, member, task, result_digest, accepted_task_digest),
            _ => return Err(invalid()),
        };
        let (result, current_digest) = self.collected(scope, *member, *task)?;
        if current_digest != *digest || result.source.accepted_task_digest != *accepted {
            return Err(error("preimage_stale", "content", Effect::NotCalled));
        }
        self.read_collected(scope, *member, *task, &result)
    }

    fn read_collected(
        &self,
        scope: &str,
        member: Uuid,
        task: Uuid,
        result: &crate::delegation_identity::CompleteResult,
    ) -> Result<ReadMaterialized, MachineError> {
        let content = self.read_content(result, task)?;
        if self.collected(scope, member, task)?.0 != *result {
            return Err(error("preimage_stale", "content", Effect::NotCalled));
        }
        Ok(content)
    }

    fn fresh(
        &self,
        scope_ref: &str,
        member: Uuid,
        task: Uuid,
    ) -> Result<DelegationIdentity, MachineError> {
        let scope = DelegationScope {
            view: &self.owner.view,
            state_root: &self.owner.state_root,
            controller: &self.owner.controller,
            trusted_invocation: None,
        };
        let caller = crate::caller_scope::TrustedCallerScope::from_port(
            &crate::trusted_host::ConfiguredCallerPort(self.owner),
        )?;
        let id = Uuid::parse_str(scope_ref).map_err(|_| invalid())?;
        let configuration = &self.owner.configuration;
        let result = match &configuration.owner {
            OwnerKind::External => {
                self.owner
                    .permit(Some(("external_engagement", scope_ref)))?;
                service::get_external_delegation_identity(
                    &scope,
                    &caller,
                    self.port,
                    service::CollectExternalSpecialistTask {
                        engagement_id: id,
                        member_ref: member,
                        task_ref: task,
                        adapter_id: &configuration.adapter_id,
                        adapter_version: &configuration.adapter_version,
                    },
                )?
            }
            OwnerKind::Managed { session_ref } if *session_ref == id => {
                self.owner
                    .permit(Some(("orchestrated_session", scope_ref)))?;
                let context = self.managed.as_ref().ok_or_else(refusal)?;
                service::get_managed_delegation_identity(
                    &scope,
                    context,
                    &caller,
                    self.port,
                    service::CollectSpecialistTask {
                        member_ref: member,
                        task_ref: task,
                        adapter_id: &configuration.adapter_id,
                        adapter_version: &configuration.adapter_version,
                    },
                )?
            }
            _ => return Err(refusal()),
        };
        result.identity.validate()?;
        self.owner.revalidate()?;
        Ok(result.identity)
    }
    pub(crate) fn select(
        &self,
        store: &Store<'_>,
        scope: &str,
        member: Uuid,
        task: Uuid,
    ) -> Result<String, MachineError> {
        store.authorize_source(self.owner, self.managed.as_ref())?;
        let identity = self.fresh(scope, member, task)?;
        let selected = Selection::Result {
            scope: scope.into(),
            member,
            task,
            result_digest: identity.result_digest.clone(),
            accepted_task_digest: identity.accepted_task_digest.clone(),
            projection: projection(&identity)?,
        };
        self.materialize(&selected)?;
        store.retain_selection(&selected)
    }
    fn materialize(&self, selected: &Selection) -> Result<Materialized, MachineError> {
        let (scope, member, task, result_digest, accepted_task_digest, expected) = match selected {
            Selection::CollectedResult {
                scope,
                member,
                task,
                result_digest,
                accepted_task_digest,
            } => (
                scope,
                member,
                task,
                result_digest,
                accepted_task_digest,
                None,
            ),
            Selection::Result {
                scope,
                member,
                task,
                result_digest,
                accepted_task_digest,
                projection,
            } => (
                scope,
                member,
                task,
                result_digest,
                accepted_task_digest,
                Some(projection),
            ),
            _ => return Err(invalid()),
        };
        let fresh = self.fresh(scope, *member, *task)?;
        let opening_projection = projection(&fresh)?;
        if fresh.result_digest != *result_digest
            || fresh.accepted_task_digest != *accepted_task_digest
            || expected.is_some_and(|expected| opening_projection != *expected)
        {
            return Err(error("preimage_stale", "content", Effect::NotCalled));
        }
        let content = self.read_content(&fresh.complete_result, *task)?;
        let closing = self.fresh(scope, *member, *task)?;
        if projection(&closing)? != opening_projection {
            return Err(error("preimage_stale", "content", Effect::NotCalled));
        }
        Ok(Materialized {
            identity: closing,
            views: content.views,
            accepted_contexts: content.accepted_contexts,
        })
    }

    fn read_content(
        &self,
        result: &crate::delegation_identity::CompleteResult,
        task: Uuid,
    ) -> Result<ReadMaterialized, MachineError> {
        let accepted = service::original_delegation_task(&self.owner.state_root, result)?;
        let accepted_contexts = accepted
            .contexts
            .into_iter()
            .map(|(_, bytes)| bytes)
            .collect();
        let mut views = BTreeMap::new();
        views.insert(view_key(&View::CompleteResult)?, encode(&result)?);
        for (structured, view, reference) in [
            (false, View::FinalResponse, &result.final_response),
            (true, View::StructuredOutput, &result.structured_output),
        ] {
            if let Some(reference) = reference {
                let retained = service::original_delegation_response(
                    &self.owner.state_root,
                    result,
                    structured,
                )?;
                let projected = if structured
                    && result.structured_output_schema.as_deref() == Some("structured_review_v3")
                {
                    let raw = views
                        .get(&view_key(&View::FinalResponse)?)
                        .ok_or_else(source_unavailable)?;
                    let task: crate::task_request::SpecialistTaskRequest =
                        serde_json::from_slice(&accepted.task_body)
                            .map_err(|_| source_integrity())?;
                    let text = std::str::from_utf8(raw).map_err(|_| source_integrity())?;
                    let output = crate::specialist::parse_reviewer_output_v3(Some(text), &task)
                        .map_err(|_| source_integrity())?;
                    Some(crate::trusted_host::canonical(&output)?)
                } else {
                    None
                };
                if retained
                    .as_ref()
                    .zip(projected.as_ref())
                    .is_some_and(|(a, b)| a != b)
                {
                    return Err(source_integrity());
                }
                let bytes = retained.or(projected).ok_or_else(source_unavailable)?;
                verify_bytes(reference, &bytes)?;
                if std::str::from_utf8(&bytes).is_err() {
                    return Err(source_integrity());
                }
                views.insert(view_key(&view)?, bytes);
            }
        }
        if !result.attachments.is_empty() {
            return Err(source_integrity());
        }
        let store = crate::engagement::EngagementStore::open_read_only(
            &crate::engagement::EngagementStore::workspace_database_path(&self.owner.state_root),
        )?;
        for (index, change) in result.changes.iter().enumerate() {
            for (side, reference, path) in [
                (
                    Side::Before,
                    &change.before,
                    change.old_path.as_deref().unwrap_or(&change.path),
                ),
                (Side::After, &change.after, change.path.as_str()),
            ] {
                if let Some(reference) = reference {
                    let bytes = crate::engagement::retained_change_side(
                        store.connection(),
                        task,
                        path,
                        match side {
                            Side::Before => "before",
                            Side::After => "after",
                        },
                    )?
                    .ok_or_else(source_unavailable)?;
                    verify_bytes(reference, &bytes)?;
                    views.insert(
                        view_key(&View::Change {
                            index: index as u64,
                            side,
                        })?,
                        bytes,
                    );
                }
            }
        }
        drop(store);
        // v4 keeps the producer's normalized verdict and response as separate
        // representations; it cannot use the v3 reconstruction above.
        if result.structured_output_schema.as_deref() == Some("structured_review_v4") {
            let bytes = views
                .get(&view_key(&View::StructuredOutput)?)
                .ok_or_else(source_unavailable)?;
            let normalized: Value = decode(bytes, bytes.len())?;
            crate::review_output::check_structure_v4(&normalized)
                .map_err(|_| source_integrity())?;
        }
        Ok(ReadMaterialized {
            result: result.clone(),
            views,
            accepted_contexts,
        })
    }
}
struct ReadMaterialized {
    result: crate::delegation_identity::CompleteResult,
    views: BTreeMap<String, Vec<u8>>,
    accepted_contexts: Vec<Vec<u8>>,
}
struct Materialized {
    identity: DelegationIdentity,
    views: BTreeMap<String, Vec<u8>>,
    accepted_contexts: Vec<Vec<u8>>,
}
fn view_key(view: &View) -> Result<String, MachineError> {
    String::from_utf8(encode(view)?).map_err(|_| invalid())
}
fn verify_bytes(reference: &BytesRef, bytes: &[u8]) -> Result<(), MachineError> {
    if *reference != BytesRef::from_bytes(bytes) {
        Err(source_integrity())
    } else {
        Ok(())
    }
}
fn source_unavailable() -> MachineError {
    error("source_unavailable", "content", Effect::NotCalled)
}
fn source_integrity() -> MachineError {
    error("source_integrity_failed", "content", Effect::NotCalled)
}

pub(crate) fn execution(i: &DelegationIdentity) -> Result<Value, MachineError> {
    i.validate()?;
    Ok(
        json!({"binding_ref":i.binding_ref,"adapter_id":i.adapter_id,"adapter_version":i.adapter_version,
        "primary_host":i.primary_host,"scope_kind":i.scope_kind,"scope_ref":i.scope_ref,"supervisor_ref":i.supervisor_ref,
        "worker_ref":i.worker_ref,"task_ref":i.task_ref,"role_ref":i.role_ref,"role_snapshot_digest":i.role_snapshot_digest,
        "agent_configuration_digest":i.agent_configuration_digest,"accepted_task_digest":i.accepted_task_digest}),
    )
}
fn podway_execution(i: &DelegationIdentity) -> Result<Value, MachineError> {
    if !podway::item_id(&i.binding_ref)
        || [&i.adapter_id, &i.adapter_version, &i.scope_kind]
            .iter()
            .any(|v| {
                v.is_empty() || v.len() > 128 || !v.bytes().all(|b| (0x21..=0x7e).contains(&b))
            })
        || [
            &i.scope_ref,
            &i.supervisor_ref,
            &i.worker_ref,
            &i.task_ref,
            &i.role_ref,
        ]
        .iter()
        .any(|v| {
            v.is_empty()
                || v.len() > 256
                || v.chars()
                    .any(|c| c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c))
        })
    {
        return Err(invalid());
    }
    execution(i)
}
fn projection(i: &DelegationIdentity) -> Result<Value, MachineError> {
    Ok(
        json!({"execution":execution(i)?,"rounds_used":i.rounds_used,
    "result_digest":i.result_digest,"worker_disposition":i.worker_disposition}),
    )
}

fn read(
    client: &mut dyn Client,
    c: &PodwayConnection,
    command: ReadCommand,
) -> Result<Value, MachineError> {
    let reply = client.invoke(c, Invocation::Read(command), &[], &mut |_| Ok(()))?;
    if !reply.success {
        return Err(error(
            "post_read_failure",
            "consumer_read",
            Effect::NotCalled,
        ));
    }
    Ok(reply.value)
}
fn snapshot(client: &mut dyn Client, c: &PodwayConnection) -> Result<(Value, Value), MachineError> {
    let manifest = read(client, c, ReadCommand::Manifest)?;
    if manifest["result"]["contract_manifest_digest"] != c.manifest_digest {
        return Err(source_integrity());
    }
    let observation = read(client, c, ReadCommand::Observe)?;
    let package = read(client, c, ReadCommand::Package)?;
    validate_snapshot(c, &observation, &package)?;
    Ok((observation, package))
}
fn validate_snapshot(
    c: &PodwayConnection,
    observation: &Value,
    package: &Value,
) -> Result<(), MachineError> {
    let r = &observation["result"];
    let p = &package["result"];
    let body = &p["package"];
    if observation["workspace"]["uuid"] != c.workspace.to_string()
        || package["workspace"]["uuid"] != c.workspace.to_string()
        || r["schema"] != "podway.observation-result/v5"
        || r["status"]["session"]["id"] != c.session.to_string()
        || r["status"]["session"]["lifecycle"] != "running"
        || r["status"]["current"]["attempt"]["attempt_id"] != c.attempt.to_string()
        || r["status"]["current"]["node"]["graph_node_id"] != c.placement
        || r["active_items_truncated"] != false
        || r["active_items"].as_array().is_none_or(|a| {
            a.len() > 128 || Some(a.len() as u64) != r["active_items_total"].as_u64()
        })
        || p["schema"] != "podway.delegation-package-result/v1"
        || body["schema"] != "podway.delegation-work-package/v1"
        || p["session_revision"].as_u64().is_none()
        || p["session_revision"] != r["status"]["session"]["revision"]
        || ["workspace_uuid", "session_id", "attempt_id"]
            .iter()
            .any(|key| p[*key] != body[*key])
        || p["workspace_uuid"] != c.workspace.to_string()
        || p["session_id"] != c.session.to_string()
        || p["attempt_id"] != c.attempt.to_string()
        || body["placement_id"] != c.placement
        || p["package_digest"].as_str().is_none_or(|s| !is_digest(s))
        || p["package_digest"] != package_digest(body)?
    {
        return Err(error("context_stale", "consumer_read", Effect::NotCalled));
    }
    let mut ids = BTreeSet::new();
    for item in r["active_items"].as_array().ok_or_else(invalid)? {
        let id = item["item_id"].as_str().ok_or_else(invalid)?;
        if !podway::item_id(id) || !ids.insert(id) || item["revision"].as_u64().is_none() {
            return Err(source_integrity());
        }
    }
    Ok(())
}
fn package_digest(body: &Value) -> Result<String, MachineError> {
    let mut bytes = b"podway.delegation-work-package/v1\n".to_vec();
    bytes.extend(encode(body)?);
    Ok(hash(&bytes))
}
fn recorded_value(kind: &str, data: Value) -> Result<Value, MachineError> {
    // Selected workflow_v2::item_value_json_v2 owns these digest bytes;
    // evidence.read's artifact representation uses different field names.
    Ok(match kind {
        "confirm" => {
            if data != true {
                return Err(source_integrity());
            }
            json!({"kind":"confirm"})
        }
        "text" => {
            let v = data.as_str().ok_or_else(source_integrity)?;
            if v.chars().count() > 65_536 {
                return Err(source_integrity());
            }
            json!({"kind":kind,"value":data})
        }
        "choice" => {
            let v = data.as_str().ok_or_else(source_integrity)?;
            if !(1..=120).contains(&v.chars().count()) {
                return Err(source_integrity());
            }
            json!({"kind":kind,"value":data})
        }
        "integer" => {
            if data.as_i64().is_none() {
                return Err(source_integrity());
            }
            json!({"kind":kind,"value":data})
        }
        "list" => {
            let values = data.as_array().ok_or_else(source_integrity)?;
            let mut total = 0usize;
            if values.len() > 1000 {
                return Err(source_integrity());
            }
            for value in values {
                let n = value.as_str().ok_or_else(source_integrity)?.chars().count();
                if !(1..=8192).contains(&n) {
                    return Err(source_integrity());
                }
                total = total.checked_add(n).ok_or_else(source_integrity)?;
            }
            if total > 1_000_000 {
                return Err(source_integrity());
            }
            json!({"kind":kind,"value":data})
        }
        "artifact" => {
            let location = data["location"].as_str().ok_or_else(source_integrity)?;
            let media = data["media_type"].as_str().ok_or_else(source_integrity)?;
            let digest = data["sha256_digest"]
                .as_str()
                .filter(|s| is_digest(s))
                .ok_or_else(source_integrity)?;
            let location_kind = match data["location_type"].as_str() {
                Some("path") => "local_path",
                Some("reference") => "external_reference",
                _ => return Err(source_integrity()),
            };
            if data.as_object().is_none_or(|o| o.len() != 5)
                || !(1..=4000).contains(&location.chars().count())
                || !(1..=255).contains(&media.chars().count())
                || data["size_bytes"].as_u64().is_none()
            {
                return Err(source_integrity());
            }
            json!({"kind":"artifact","location_kind":location_kind,"location":location,"digest":digest,"size_bytes":data["size_bytes"],"media_type":media})
        }
        "check_result" => {
            let mut value = data;
            let object = value.as_object_mut().ok_or_else(source_integrity)?;
            if object.contains_key("type") {
                return Err(source_integrity());
            }
            object.insert("type".into(), json!(kind));
            let record: RecordValue = decode(&encode(&value)?, 32768)?;
            record.validate()?;
            value
                .as_object_mut()
                .ok_or_else(source_integrity)?
                .remove("type");
            value["kind"] = json!(kind);
            value
        }
        _ => return Err(source_integrity()),
    })
}
fn preimage(observation: &Value, package: &Value, projection: Option<Value>) -> Value {
    // Public revision fences bind item state. Keep only those facts and the
    // selected package digest, never another full package/result in the store.
    let items = observation["result"]["active_items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|i| json!({"item_id":i["item_id"],"revision":i["revision"]}))
                .collect::<Vec<_>>()
        });
    json!({"workspace":observation["workspace"]["uuid"],"session":observation["result"]["status"]["session"],
        "current":observation["result"]["status"]["current"],"items":items,
        "package":{"package_digest":package["result"]["package_digest"],"session_revision":package["result"]["session_revision"]},"projection":projection})
}
fn verify_original_package(material: &Materialized, package: &Value) -> Result<(), MachineError> {
    podway_execution(&material.identity)?;
    verify_captured_package(&material.accepted_contexts, package)
}
fn verify_captured_package(contexts: &[Vec<u8>], package: &Value) -> Result<(), MachineError> {
    let body = &package["result"]["package"];
    let canonical = encode(body)?;
    if !contexts.iter().any(|bytes| {
        // Input capture preserves original bytes. Whitespace and key order may
        // differ, while the complete selected package must remain identical.
        decode::<Value>(bytes, 1_000_000)
            .is_ok_and(|value| value == *body || value["result"]["package"] == *body)
    }) || package_digest(body)? != package["result"]["package_digest"]
        || canonical.len() > 1_000_000
    {
        return Err(error(
            "preimage_stale",
            "accepted_package",
            Effect::NotCalled,
        ));
    }
    Ok(())
}
fn accept_input(
    c: &PodwayConnection,
    p: &Value,
    i: &DelegationIdentity,
    key: &str,
    summary: &str,
) -> Result<Vec<u8>, MachineError> {
    let d = &p["result"]["package"]["delegation"];
    let e = podway_execution(i)?;
    if i.adapter_id != c.adapter_id
        || i.adapter_version != c.adapter_version
        || ["binding_ref", "role_ref", "role_snapshot_digest"]
            .iter()
            .any(|k| e[*k] != d[*k])
        || d["max_rounds"].as_u64().is_none_or(|n| i.rounds_used > n)
        || !matches!(
            (d["after_accept"].as_str(), i.worker_disposition.as_str()),
            (Some("retain"), "idle") | (Some("release"), "released")
        )
        || i.caller_scope.podway_workspace_ref != c.workspace.to_string()
        || i.caller_scope.podway_session_ref != c.session.to_string()
        || i.caller_scope.podway_attempt_ref != c.attempt.to_string()
        || i.caller_scope.delegated_placement_ref != c.placement
    {
        return Err(source_integrity());
    }
    let bytes = encode(
        &json!({"schema":"podway.delegation-accept-input/v1","workspace_uuid":c.workspace,"session_id":c.session,
        "session_revision":p["result"]["session_revision"],"attempt_id":c.attempt,"package_digest":p["result"]["package_digest"],
        "idempotency_key":key,"execution":e,"rounds_used":i.rounds_used,"result_digest":i.result_digest,
        "summary":summary,"worker_disposition":i.worker_disposition,"accepted":true}),
    )?;
    if bytes.len() > 32768 {
        return Err(invalid());
    }
    Ok(bytes)
}

fn declared_record(
    value: &RecordValue,
    declaration: &Value,
    artifact: Option<Value>,
    evidence: &Materialized,
) -> Result<Value, MachineError> {
    let mut record = serde_json::to_value(value).map_err(|_| invalid())?;
    if record["type"] != declaration["type"] {
        return Err(invalid());
    }
    match value {
        RecordValue::Integer { value } => {
            let n = value.parse::<i64>().map_err(|_| invalid())?;
            if declaration["minimum"].as_i64().is_some_and(|v| n < v)
                || declaration["maximum"].as_i64().is_some_and(|v| n > v)
            {
                return Err(invalid());
            }
            record["value"] = json!(n);
        }
        RecordValue::Text { value } => {
            let n = value.chars().count() as u64;
            if declaration["min_length"].as_u64().is_some_and(|v| n < v)
                || declaration["max_length"].as_u64().is_some_and(|v| n > v)
                || declaration["multiline"] == false && value.contains(['\n', '\r'])
            {
                return Err(invalid());
            }
        }
        RecordValue::Choice { value } => {
            if declaration["choices"]
                .as_array()
                .is_none_or(|a| !a.contains(&json!(value)))
            {
                return Err(invalid());
            }
        }
        RecordValue::List { value } => {
            let n = value.len() as u64;
            let total = value
                .iter()
                .try_fold(0u64, |n, v| n.checked_add(v.chars().count() as u64))
                .ok_or_else(invalid)?;
            if declaration["min_items"].as_u64().is_some_and(|v| n < v)
                || declaration["max_items"].as_u64().is_some_and(|v| n > v)
                || value.iter().any(|v| {
                    declaration["max_item_length"]
                        .as_u64()
                        .is_some_and(|n| v.chars().count() as u64 > n)
                })
                || declaration["max_total_length"]
                    .as_u64()
                    .is_some_and(|v| total > v)
                || declaration["unique"].as_bool().unwrap_or(true)
                    && value.iter().collect::<BTreeSet<_>>().len() != value.len()
            {
                return Err(invalid());
            }
        }
        RecordValue::Artifact { .. } => {
            record = artifact.ok_or_else(invalid)?;
            if declaration["allowed_media_types"]
                .as_array()
                .is_some_and(|v| !v.contains(&record["media_type"]))
            {
                return Err(invalid());
            }
        }
        RecordValue::CheckResult {
            operation_id,
            operation_digest,
            output_digest,
            ..
        } if declaration["operation_id"] != *operation_id
            || declaration["operation_digest"] != *operation_digest
            || declaration["accepted_outcomes"]
                .as_array()
                .is_none_or(|a| !a.contains(&record["outcome"]))
            || !evidence
                .views
                .values()
                .any(|bytes| hash(bytes) == *output_digest)
            || (record["input_basis"]["digest"] != evidence.identity.accepted_task_digest
                && !evidence
                    .views
                    .values()
                    .any(|bytes| json!(hash(bytes)) == record["input_basis"]["digest"])) =>
        {
            return Err(source_integrity());
        }
        _ => {}
    }
    Ok(record)
}

pub(crate) struct Bridge<'a> {
    pub store: Store<'a>,
    pub source: ProductSource<'a>,
}
impl Bridge<'_> {
    pub(crate) fn handle(&self, client: &mut dyn Client, control: &Control) -> Value {
        let mut effect = Effect::NotCalled;
        let result = self.dispatch(client, control, &mut effect);
        match result {
            Ok(data) => {
                json!({"schema":"dolgorae.consumer-control-response/v1","action":control.name(),"ok":true,"request_id":control.request_id(),"data":data})
            }
            Err(e) => {
                let code = e
                    .details
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("unauthorized");
                let phase = e
                    .details
                    .get("phase")
                    .and_then(Value::as_str)
                    .unwrap_or("authority");
                let code = if effect == Effect::Committed {
                    "post_read_failure"
                } else {
                    code
                };
                json!({"schema":"dolgorae.consumer-control-response/v1","action":control.name(),"ok":false,"request_id":control.request_id(),
                    "error":{"code":code,"phase":phase,"effect":effect,"recovery":if effect==Effect::Unknown{"lookup_original_request"}else if effect==Effect::Committed{"inspect_historical_outcome"}else{"fix_request"}}})
            }
        }
    }
    fn dispatch(
        &self,
        client: &mut dyn Client,
        control: &Control,
        effect: &mut Effect,
    ) -> Result<Value, MachineError> {
        self.store
            .authorize_source(self.source.owner, self.source.managed.as_ref())?;
        match &control.action {
            Action::Status {
                target: StatusTarget::Context,
            } => match self.store.connection() {
                Err(_) => Ok(json!({"kind":"context","mode":"native_only","context_ref":null})),
                Ok(c) => {
                    podway::validate_connection(self.source.owner, c)?;
                    let (o, p) = snapshot(client, c)?;
                    let reference = self.store.retain_selection(&Selection::Context {
                        preimage: preimage(&o, &p, None),
                    })?;
                    Ok(json!({"kind":"context","mode":"delegated","context_ref":reference}))
                }
            },
            Action::Status {
                target: StatusTarget::Request { request_id },
            } => {
                let r = self.recover(client, request_id, effect)?;
                self.status(r)
            }
            Action::Inspect {
                selection_ref,
                view,
                range,
            } => self.inspect(client, selection_ref, view, range, effect),
            Action::PrepareAccept {
                request_id,
                evidence_ref,
                summary,
            } => {
                let reserved = self.store.reserve(control)?;
                *effect = reserved.effect;
                if reserved.state != State::Preparing {
                    return self.status(reserved);
                }
                let c = self.store.connection()?;
                podway::validate_connection(self.source.owner, c)?;
                let selected: Selection = self.store.selection(evidence_ref)?;
                let material = self.source.materialize(&selected)?;
                let (o, p) = snapshot(client, c)?;
                verify_original_package(&material, &p)?;
                self.consume_package_evidence(client, c, &p)?;
                let input = accept_input(c, &p, &material.identity, &reserved.key, summary)?;
                let request = self.prepared(
                    request_id,
                    evidence_ref,
                    Mutation::Accept,
                    input,
                    preimage(&o, &p, Some(projection(&material.identity)?)),
                )?;
                *effect = request.effect;
                self.status(request)
            }
            Action::PrepareRecord {
                request_id,
                context_ref,
                evidence_ref,
                operations,
            } => {
                let reserved = self.store.reserve(control)?;
                *effect = reserved.effect;
                if reserved.state != State::Preparing {
                    return self.status(reserved);
                }
                let c = self.store.connection()?;
                podway::validate_connection(self.source.owner, c)?;
                let Selection::Context { preimage: expected } =
                    self.store.selection(context_ref)?
                else {
                    return Err(invalid());
                };
                let (o, p) = snapshot(client, c)?;
                if preimage(&o, &p, None) != expected {
                    return Err(error("context_stale", "prepare", Effect::NotCalled));
                }
                let selected: Selection = self.store.selection(evidence_ref)?;
                let material = self.source.materialize(&selected)?;
                verify_original_package(&material, &p)?;
                let declarations = p["result"]["package"]["action"]["items"]
                    .as_array()
                    .ok_or_else(source_integrity)?;
                let items = o["result"]["active_items"]
                    .as_array()
                    .ok_or_else(source_integrity)?;
                let mut output = Vec::new();
                for op in operations {
                    if !c.permitted_items.contains(op.item()) {
                        return Err(error("unauthorized", "record", Effect::NotCalled));
                    }
                    let item = items
                        .iter()
                        .find(|v| v["item_id"] == op.item())
                        .ok_or_else(invalid)?;
                    let declaration = declarations
                        .iter()
                        .find(|v| v["id"] == op.item())
                        .ok_or_else(invalid)?;
                    let mut result =
                        json!({"item_id":op.item(),"expected_item_revision":item["revision"]});
                    match op {
                        RecordOperation::Clear(_) => result["clear"] = json!(true),
                        RecordOperation::Record(v) => {
                            let artifact = if let RecordValue::Artifact { artifact_ref } = &v.record
                            {
                                Some(self.artifact(artifact_ref, evidence_ref, &material)?)
                            } else {
                                None
                            };
                            result["record"] =
                                declared_record(&v.record, declaration, artifact, &material)?;
                        }
                    }
                    output.push(result);
                }
                let input = encode(
                    &json!({"schema":"podway.item-record-many-input/v1","workspace_uuid":c.workspace,"session_id":c.session,
                    "session_revision":o["result"]["status"]["session"]["revision"],"attempt_id":c.attempt,"idempotency_key":reserved.key,"operations":output}),
                )?;
                if input.len() > MAX_INPUT {
                    return Err(invalid());
                }
                let request = self.prepared(
                    request_id,
                    evidence_ref,
                    Mutation::Record,
                    input,
                    preimage(&o, &p, Some(projection(&material.identity)?)),
                )?;
                *effect = request.effect;
                self.status(request)
            }
            Action::Record { request_id, .. } => {
                self.commit(client, request_id, Mutation::Record, effect)
            }
            Action::Accept { request_id, .. } => {
                self.commit(client, request_id, Mutation::Accept, effect)
            }
        }
    }
    fn prepared(
        &self,
        id: &str,
        evidence: &str,
        mutation: Mutation,
        input: Vec<u8>,
        preimage: Value,
    ) -> Result<Request, MachineError> {
        let selection_ref = String::new();
        let request_digest = podway::request_digest(mutation, &input)?;
        let r = self.store.prepare(
            id,
            Prepared {
                mutation,
                input,
                preimage,
                evidence_ref: evidence.into(),
                selection_ref,
                request_digest,
            },
        )?;
        Ok(r)
    }
    fn status(&self, r: Request) -> Result<Value, MachineError> {
        let input = match r.prepared.as_ref() {
            Some(p) => Some(
                json!({"digest":hash(&p.input),"byte_length":p.input.len(),"selection_ref":p.selection_ref,
                "inline_input":if p.input.len()<=1024{Some(std::str::from_utf8(&p.input).map_err(|_|source_integrity())?)}else{None}}),
            ),
            None => None,
        };
        let retained = |value: &Option<Value>| -> Result<Option<Value>, MachineError> {
            value.as_ref().map(|value|{let bytes=encode(value)?;Ok(json!({"selection_ref":r.prepared.as_ref().map(|p|&p.selection_ref),"digest":hash(&bytes),"byte_length":bytes.len()}))}).transpose()
        };
        let outcome = retained(&r.outcome)?;
        let receipt = retained(&r.receipt)?;
        Ok(
            json!({"kind":if r.state==State::Prepared{"prepared_request"}else{"request_status"},"request_id":r.request_id,
            "state":r.state,"effect":r.effect,"input":input,"outcome":outcome,"receipt":receipt,"applicability":r.applicability}),
        )
    }
    fn commit(
        &self,
        client: &mut dyn Client,
        id: &str,
        mutation: Mutation,
        effect: &mut Effect,
    ) -> Result<Value, MachineError> {
        let r = self.store.request(id)?;
        *effect = r.effect;
        if r.prepared.as_ref().is_none_or(|p| p.mutation != mutation) {
            return Err(invalid());
        }
        if r.state != State::Prepared {
            return self.status(self.recover(client, id, effect)?);
        }
        let p = r.prepared.as_ref().ok_or_else(invalid)?;
        let c = self.store.connection()?;
        podway::validate_connection(self.source.owner, c)?;
        let selected: Selection = self.store.selection(&p.evidence_ref)?;
        let material = self.source.materialize(&selected)?;
        let (o, package) = snapshot(client, c)?;
        verify_original_package(&material, &package)?;
        self.consume_package_evidence(client, c, &package)?;
        if p.preimage != preimage(&o, &package, Some(projection(&material.identity)?)) {
            return Err(error("preimage_stale", "commit", Effect::NotCalled));
        }
        if mutation == Mutation::Accept {
            let input: Value = decode(&p.input, 32768)?;
            if accept_input(
                c,
                &package,
                &material.identity,
                &r.key,
                input["summary"].as_str().ok_or_else(invalid)?,
            )? != p.input
            {
                return Err(source_integrity());
            }
        }
        self.store.authorize()?;
        let (r, claim) = self.store.claim(id, mutation)?;
        *effect = r.effect;
        let Some(claim) = claim else {
            return self.status(r);
        };
        // From the durable claim onward, a later failed state read cannot prove
        // non-dispatch. A verified child result refines this local effect.
        *effect = Effect::Unknown;
        let reply = client.invoke(c, Invocation::Mutate(mutation), &p.input, &mut |pid| {
            self.store.running(id, claim, pid)
        });
        match reply {
            Ok(reply) if reply.success => {
                if podway::verify_success(p, &reply.value).is_err() {
                    self.store
                        .finish(id, claim, Effect::Unknown, Some(reply.value))?;
                    return Err(error("uncertain_outcome", "result", Effect::Unknown));
                }
                *effect = Effect::Committed;
                self.store
                    .finish(id, claim, Effect::Committed, Some(reply.value))?;
            }
            Ok(reply) => {
                let result_effect = if reply.value["code"] == "MUTATION_OUTCOME_UNKNOWN" {
                    Effect::Unknown
                } else {
                    Effect::Rejected
                };
                *effect = result_effect;
                self.store
                    .finish(id, claim, result_effect, Some(reply.value))?;
                return Err(error(
                    if result_effect == Effect::Unknown {
                        "uncertain_outcome"
                    } else {
                        "consumer_rejection"
                    },
                    "commit",
                    result_effect,
                ));
            }
            Err(_) => {
                self.store.finish(id, claim, Effect::Unknown, None)?;
                return Err(error("uncertain_outcome", "child", Effect::Unknown));
            }
        }
        self.status(self.recover(client, id, effect)?)
    }
    fn recover(
        &self,
        client: &mut dyn Client,
        id: &str,
        effect: &mut Effect,
    ) -> Result<Request, MachineError> {
        let mut r = self.store.request(id)?;
        *effect = r.effect;
        let original_outcome = (r.state == State::Committed)
            .then(|| r.outcome.clone())
            .flatten();
        let original_receipt = (r.state == State::Committed)
            .then(|| r.receipt.clone())
            .flatten();
        if !matches!(
            r.state,
            State::Claimed | State::Running | State::Uncertain | State::Committed
        ) {
            return Ok(r);
        }
        let c = self.store.connection()?;
        podway::validate_connection(self.source.owner, c)?;
        // Recovery precedes fresh preimage validation. A later stale placement
        // cannot erase the historical result of the original request.
        let reply = client.invoke(
            c,
            Invocation::Read(ReadCommand::Outcome(r.key.clone())),
            &[],
            &mut |_| Ok(()),
        )?;
        let outcome = podway::outcome(&r, &reply)?;
        if original_outcome
            .as_ref()
            .zip(outcome.as_ref())
            .is_some_and(|(a, b)| a != b)
        {
            return Err(error(
                "source_integrity_failed",
                "outcome",
                Effect::Committed,
            ));
        }
        if outcome.is_some() {
            *effect = Effect::Committed;
        }
        r = self.store.recover(id, outcome, None, None)?;
        if r.prepared
            .as_ref()
            .is_some_and(|p| p.mutation == Mutation::Accept)
        {
            let reply =
                client.invoke(c, Invocation::Read(ReadCommand::Receipt), &[], &mut |_| {
                    Ok(())
                })?;
            if reply.success {
                let (receipt, applicability) = podway::receipt(&r, c, &reply)?;
                let receipt = if let Some(original) = original_receipt {
                    if original["result"]["receipt"] != receipt["result"]["receipt"] {
                        return Err(error(
                            "source_integrity_failed",
                            "receipt",
                            Effect::Committed,
                        ));
                    }
                    original
                } else {
                    receipt
                };
                // A receipt establishes applicability, but contains no original
                // key. An expired/missing lookup remains uncertain without an
                // already verified original outcome, even for matching input.
                r = self
                    .store
                    .recover(id, None, Some(receipt), Some(applicability))?;
            } else if r.state == State::Committed {
                return Err(error("post_read_failure", "receipt", Effect::Committed));
            }
        }
        let current = read(client, c, ReadCommand::Observe)?;
        let applicable = current["result"]["status"]["session"]["id"] == c.session.to_string()
            && current["result"]["status"]["current"]["attempt"]["attempt_id"]
                == c.attempt.to_string();
        if r.applicability.is_none() {
            r = self.store.recover(
                id,
                None,
                None,
                Some(if applicable { "current" } else { "historical" }.into()),
            )?;
        }
        Ok(r)
    }
    pub(crate) fn artifact_selection(
        &self,
        evidence_ref: &str,
        view: View,
        media_type: &str,
    ) -> Result<String, MachineError> {
        if media_type.is_empty() || media_type.len() > 255 {
            return Err(invalid());
        }
        let selected: Selection = self.store.selection(evidence_ref)?;
        let material = self.source.materialize(&selected)?;
        let bytes = material.views.get(&view_key(&view)?).ok_or_else(invalid)?;
        self.store.retain_selection(&Selection::Artifact {
            evidence_ref: evidence_ref.into(),
            view,
            source: BytesRef::from_bytes(bytes),
            media_type: media_type.into(),
        })
    }
    fn artifact(
        &self,
        reference: &str,
        evidence_ref: &str,
        material: &Materialized,
    ) -> Result<Value, MachineError> {
        let Selection::Artifact {
            evidence_ref: original,
            view,
            source,
            media_type,
        } = self.store.selection(reference)?
        else {
            return Err(invalid());
        };
        if original != evidence_ref {
            return Err(source_integrity());
        }
        let bytes = material
            .views
            .get(&view_key(&view)?)
            .ok_or_else(source_unavailable)?;
        verify_bytes(&source, bytes)?;
        let task = &material.identity.task_ref;
        Ok(
            json!({"type":"artifact","reference":format!("dolgorae:task:{task}:{reference}"),"digest":source.sha256,"size_bytes":source.byte_length,"media_type":media_type}),
        )
    }
    fn inspect(
        &self,
        client: &mut dyn Client,
        reference: &str,
        view: &View,
        range: &Range,
        effect: &mut Effect,
    ) -> Result<Value, MachineError> {
        let selected: Selection = self.store.selection(reference)?;
        let bytes = match selected {
            Selection::Result { .. } | Selection::CollectedResult { .. } => self
                .source
                .read_materialize(&selected)?
                .views
                .remove(&view_key(view)?)
                .ok_or_else(source_unavailable)?,
            Selection::Context { preimage: expected } => {
                let c = self.store.connection()?;
                let (o, p) = snapshot(client, c)?;
                if preimage(&o, &p, None) != expected {
                    return Err(source_unavailable());
                }
                match view {
                    View::Package => encode(&p["result"]["package"])?,
                    View::PackageEvidence { index } => self
                        .consume_package_evidence(client, c, &p)?
                        .get(usize::try_from(*index).map_err(|_| invalid())?)
                        .cloned()
                        .ok_or_else(invalid)?,
                    _ => return Err(invalid()),
                }
            }
            Selection::Request { request_id } => {
                let r = self.store.request(&request_id)?;
                *effect = r.effect;
                match view {
                    View::PreparedInput => r.prepared.ok_or_else(source_unavailable)?.input,
                    View::Outcome => encode(&r.outcome.ok_or_else(source_unavailable)?)?,
                    View::Receipt => encode(&r.receipt.ok_or_else(source_unavailable)?)?,
                    _ => return Err(invalid()),
                }
            }
            _ => return Err(invalid()),
        };
        let source = BytesRef::from_bytes(&bytes);
        let offset = match range {
            Range::Start { offset } => *offset,
            Range::Continue { cursor } => {
                let Selection::Cursor {
                    selection_ref,
                    view: original,
                    offset,
                    source: expected,
                } = self.store.selection(cursor)?
                else {
                    return Err(invalid());
                };
                if selection_ref != reference || original != *view || expected != source {
                    return Err(source_integrity());
                }
                offset
            }
        };
        let (data, encoding, end) = page(&bytes, offset)?;
        let continuation = if end < bytes.len() as u64 {
            Some(self.store.retain_selection(&Selection::Cursor {
                selection_ref: reference.into(),
                view: view.clone(),
                offset: end,
                source: source.clone(),
            })?)
        } else {
            None
        };
        Ok(
            json!({"kind":"page","source_digest":source.sha256,"source_length":source.byte_length,"offset":offset,
            "returned_length":end-offset,"encoding":encoding,"continuation":continuation,"text":data}),
        )
    }
    fn consume_package_evidence(
        &self,
        client: &mut dyn Client,
        c: &PodwayConnection,
        package: &Value,
    ) -> Result<Vec<Vec<u8>>, MachineError> {
        let mut complete = Vec::new();
        let evidence = package["result"]["package"]["evidence"]
            .as_array()
            .ok_or_else(source_integrity)?;
        for group in evidence {
            if group["resolution"] != "resolved" {
                if group["selector"]["required"] == false {
                    continue;
                }
                return Err(source_unavailable());
            }
            let source = group["selector"]["node"]
                .as_str()
                .ok_or_else(source_integrity)?;
            for item in group["items"].as_array().ok_or_else(source_integrity)? {
                let id = item["item_id"].as_str().ok_or_else(source_integrity)?;
                match item["state"].as_str() {
                    Some("missing") if item.as_object().is_some_and(|o| o.len() == 2) => {
                        continue;
                    }
                    Some("present") => {}
                    _ => return Err(source_integrity()),
                }
                let mut token = None;
                let mut seen = BTreeSet::new();
                let mut offset = 0u64;
                let mut data = Value::Null;
                let mut metadata = None;
                loop {
                    let reply = read(
                        client,
                        c,
                        ReadCommand::Evidence {
                            source: source.into(),
                            item: id.into(),
                            page: token.clone(),
                        },
                    )?;
                    if encode(&reply)?.len() > 320 * 1024 {
                        return Err(source_integrity());
                    }
                    let r = &reply["result"];
                    let current = json!({"consumer":r["consumer"],"source":r["source"],"item":r["item"],"value_digest":r["value_digest"],"total_size":r["total_size"],"size_unit":r["size_unit"],"page_token_version":r["page_token_version"]});
                    if metadata
                        .as_ref()
                        .is_some_and(|expected| *expected != current)
                    {
                        return Err(source_integrity());
                    }
                    metadata = Some(current);
                    if r["schema"] != "podway.evidence-read-result/v1"
                        || r["consumer"]["graph_node_id"] != c.placement
                        || r["consumer"]["attempt_id"] != c.attempt.to_string()
                        || r["consumer"]["attempt_number"]
                            .as_u64()
                            .is_none_or(|v| v == 0)
                        || r["source"]["graph_node_id"] != source
                        || r["source"]["attempt_number"] != group["source_attempt_number"]
                        || r["source"]["attempt_id"] != group["source_attempt_id"]
                        || r["source"]["items_digest"] != group["source_items_digest"]
                        || r["item"]["item_id"] != id
                        || r["item"]["revision"] != item["item_revision"]
                        || r["value_digest"] != item["value_digest"]
                        || r["page"]["offset"].as_u64() != Some(offset)
                    {
                        return Err(source_integrity());
                    }
                    let kind = item["value_type"].as_str().ok_or_else(source_integrity)?;
                    let unit = match kind {
                        "text" => "scalars",
                        "list" => "entries",
                        "check_result" => "bytes",
                        _ => "value",
                    };
                    let truncated = r["truncated"].as_bool().ok_or_else(source_integrity)?;
                    let size = r["page"]["size"].as_u64().ok_or_else(source_integrity)?;
                    if r["size_unit"] != unit
                        || r["page_token_version"] != 1
                        || encode(&r["page"]["data"])?.len() > 262_144
                        || (unit == "value" && (size != 1 || truncated))
                        || (unit == "bytes"
                            && (size != encode(&r["page"]["data"])?.len() as u64 || truncated))
                    {
                        return Err(source_integrity());
                    }
                    offset = offset.checked_add(size).ok_or_else(source_integrity)?;
                    match r["size_unit"].as_str() {
                        Some("scalars") => {
                            let text = r["page"]["data"].as_str().ok_or_else(source_integrity)?;
                            if text.chars().count() as u64 != size {
                                return Err(source_integrity());
                            }
                            let mut current = data.as_str().unwrap_or("").to_owned();
                            current.push_str(text);
                            data = json!(current);
                        }
                        Some("entries") => {
                            let values =
                                r["page"]["data"].as_array().ok_or_else(source_integrity)?;
                            if values.len() as u64 != size {
                                return Err(source_integrity());
                            }
                            let mut current = data.as_array().cloned().unwrap_or_default();
                            current.extend(values.iter().cloned());
                            data = json!(current);
                        }
                        Some("value" | "bytes") if token.is_none() => {
                            data = r["page"]["data"].clone()
                        }
                        _ => return Err(source_integrity()),
                    }
                    recorded_value(kind, data.clone())?;
                    if r["item"]["type"] != kind {
                        return Err(source_integrity());
                    }
                    if !truncated {
                        if !r["next_page_token"].is_null()
                            || r["total_size"].as_u64() != Some(offset)
                        {
                            return Err(source_integrity());
                        }
                        break;
                    }
                    let next = r["next_page_token"]
                        .as_str()
                        .ok_or_else(source_integrity)?
                        .to_owned();
                    if size == 0
                        || next.is_empty()
                        || next.chars().count() > 256
                        || !seen.insert(next.clone())
                    {
                        return Err(source_integrity());
                    }
                    token = Some(next);
                }
                // Podway evidence digests cover the typed item value.
                let value = recorded_value(
                    item["value_type"].as_str().ok_or_else(source_integrity)?,
                    data,
                )?;
                let bytes = encode(&value)?;
                if hash(&bytes) != item["value_digest"]
                    || item["size_bytes"].as_u64() != Some(bytes.len() as u64)
                {
                    return Err(source_integrity());
                }
                complete.push(bytes);
            }
        }
        Ok(complete)
    }
}
fn page(bytes: &[u8], offset: u64) -> Result<(String, &'static str, u64), MachineError> {
    let start = usize::try_from(offset).map_err(|_| invalid())?;
    if start > bytes.len() {
        return Err(invalid());
    }
    let mut end = start.saturating_add(1024).min(bytes.len());
    if let Ok(text) = std::str::from_utf8(bytes) {
        if !text.is_char_boundary(start) {
            return Err(invalid());
        }
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok((text[start..end].into(), "utf8", end as u64))
    } else {
        use base64::Engine as _;
        Ok((
            base64::engine::general_purpose::STANDARD.encode(&bytes[start..end]),
            "base64",
            end as u64,
        ))
    }
}

/// Native output contains bounded ASCII metadata and, for inspection, one
/// separate original-text block. The retained private/public values stay typed.
pub(crate) fn native_content(value: &Value, managed: bool) -> Result<Value, MachineError> {
    let mut metadata = value.clone();
    let original = if metadata["data"]["kind"] == "page" {
        metadata["data"]
            .as_object_mut()
            .ok_or_else(invalid)?
            .remove("text")
    } else {
        None
    };
    let json = String::from_utf8(encode(&metadata)?).map_err(|_| invalid())?;
    let mut ascii = String::new();
    for c in json.chars() {
        if c.is_ascii() {
            ascii.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]).iter() {
                use std::fmt::Write as _;
                write!(&mut ascii, "\\u{unit:04x}").map_err(|_| invalid())?;
            }
        }
    }
    if ascii.len() > 4096 {
        return Err(error(
            "presentation_unavailable",
            "metadata",
            Effect::Unknown,
        ));
    }
    let block_type = if managed { "inputText" } else { "text" };
    let mut blocks = vec![json!({"type":block_type,"text":ascii})];
    if let Some(original) = original {
        let text = original.as_str().ok_or_else(invalid)?;
        // Binary pages have a base64 encoding declared in the metadata.
        if text.len() > 1368 {
            return Err(source_integrity());
        }
        blocks.push(json!({"type":block_type,"text":text}));
    }
    Ok(json!(blocks))
}

pub(crate) fn presentation_failure(value: &Value) -> Value {
    if value["schema"] == "dolgorae.consumer-control-response/v1" {
        json!({"schema":value["schema"],"action":value["action"],"request_id":value["request_id"],"ok":false,"error":{"code":"presentation_unavailable","phase":"presentation","effect":value["error"].get("effect").or_else(||value["data"].get("effect")).cloned().unwrap_or(json!("not_called")),"recovery":"inspect original request; do not redispatch"}})
    } else {
        json!({"schema":"dolgorae.native-specialist-response/v1","operation":value["operation"],"ok":false,"semantic":value["semantic"],"facts":null,"evidence":null,"presentation_error":{"code":"presentation_unavailable","recovery":"inspect original operation; do not redispatch"}})
    }
}

pub(crate) fn native_failed(value: &Value) -> bool {
    value["ok"] == false || value["schema"] == "dolgorae.external-specialist-failure/v5"
}

/// Preserve semantic classification independently of later presentation work.
/// Larger result bodies are consumed here and selected by their existing owner.
pub(crate) fn semantic_presentation(public: &Value) -> Value {
    let failed = public["schema"] == "dolgorae.external-specialist-failure/v5";
    let operation = public["operation"].as_str().unwrap_or("lookup_operation");
    let class = crate::host_invocation::method_class(operation);
    let mut facts = serde_json::Map::new();
    for key in ["receipt", "operation_ref", "wait_timed_out"] {
        if let Some(v) = public.get(key) {
            facts.insert(key.into(), v.clone());
        }
    }
    for key in ["candidate", "engagement", "member", "task", "lookup"] {
        if let Some(body) = public.get(key) {
            let selected = match (key, body.as_object()) {
                ("task", Some(fields)) => Value::Object(
                    fields
                        .iter()
                        .filter(|(key, _)| key.as_str() != "complete_result")
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                ),
                _ => body.clone(),
            };
            facts.insert(key.into(), selected);
        }
    }
    json!({"schema":"dolgorae.native-specialist-response/v1","operation":operation,"ok":!failed,
        "semantic":{"schema":public["schema"],"classification":if failed{public["error"]["classification"].clone()}else{json!("Success")},
        "effect":if failed{public["error"]["effect"].clone()}else{json!(match class{Ok(crate::host_invocation::MethodClass::Keyed)=>"accepted",Ok(crate::host_invocation::MethodClass::State)=>"observed",_=>"none"})},
        "operation_ref":public.get("receipt").and_then(|r|r.get("operation_ref")).or_else(||public.get("operation_ref")).or_else(||public["error"].get("operation_ref")).cloned().unwrap_or(Value::Null),
        "error":if failed{public["error"].clone()}else{Value::Null}},"facts":facts,"evidence":null,"presentation_error":null})
}

impl Bridge<'_> {
    pub(crate) fn semantic(
        &self,
        client: &mut dyn Client,
        request: &Value,
        public: &Value,
    ) -> Value {
        let mut presentation = semantic_presentation(public);
        if public["schema"] == "dolgorae.external-specialist-failure/v5" {
            return presentation;
        }
        let has_result =
            public.get("identity").is_some() || public["task"]["complete_result"].is_object();
        if !has_result {
            return presentation;
        }
        let result: Result<Value, MachineError> = (|| {
            let scope = request["scope_ref"].as_str().ok_or_else(invalid)?;
            let member = Uuid::parse_str(request["member_ref"].as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let task = Uuid::parse_str(request["task_ref"].as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let (reference, material) = if public.get("identity").is_some() {
                let reference = self.source.select(&self.store, scope, member, task)?;
                let selected: Selection = self.store.selection(&reference)?;
                let material = self.source.read_materialize(&selected)?;
                (reference, material)
            } else {
                self.source
                    .select_collected(&self.store, scope, member, task)?
            };
            if let Ok(c) = self.store.connection() {
                podway::validate_connection(self.source.owner, c)?;
                let (_, package) = snapshot(client, c)?;
                verify_captured_package(&material.accepted_contexts, &package)?;
                self.consume_package_evidence(client, c, &package)?;
            }
            let raw = material.views.get(&view_key(&View::FinalResponse)?);
            let normalized = material.views.get(&view_key(&View::StructuredOutput)?);
            let relation = match (raw, normalized) {
                (Some(raw), Some(normalized)) if raw == normalized => "identical",
                (Some(raw), Some(normalized)) => {
                    let a: Option<Value> = serde_json::from_slice(raw).ok();
                    let b: Option<Value> = serde_json::from_slice(normalized).ok();
                    if a.is_some() && a == b {
                        "canonical_equivalent"
                    } else {
                        "substantive_normalization"
                    }
                }
                (Some(_), None) => "original_only",
                (None, Some(_)) => "structured_only",
                _ => "no_response",
            };
            let format = material.result.structured_output_schema.as_deref();
            let mapping = match self.source.owner.configuration.owner {
                OwnerKind::External => "dolgorae.podway-external/v1",
                OwnerKind::Managed { .. } => "dolgorae.podway-managed/v1",
            };
            Ok(
                json!({"selection_ref":reference,"result_digest":material.result.digest()?,"mapping_contract":mapping,
                "final_response":material.result.final_response,"structured_output":material.result.structured_output,
                "structured_output_schema":format,"relationship":relation,
                "interpretation":if relation=="substantive_normalization" {"normalized structured verdict differs substantively; inspect both original and normalized views, including evidence-limit disclosure"} else {"complete_result; canonical-equivalent formatting does not require duplicate inspection"},
                "instruction":"Inspect applicable original evidence through the consumer tool. Supply explicit Record/Accept decisions; emission and cursors do not prove attention."}),
            )
        })();
        match result {
            Ok(evidence) => presentation["evidence"] = evidence,
            Err(e) => {
                presentation["ok"] = json!(false);
                presentation["presentation_error"] = json!({"code":e.code,"recovery":"inspect original task or original operation; do not redispatch a semantic effect"});
            }
        }
        presentation
    }

    pub(crate) fn assignment_binding(
        &self,
        client: &mut dyn Client,
        request: &Value,
    ) -> Result<(), MachineError> {
        if !matches!(
            request["operation"].as_str(),
            Some("assign_external_specialist_task" | "assign_specialist_task")
        ) {
            return Ok(());
        }
        let Ok(c) = self.store.connection() else {
            return Ok(());
        };
        podway::validate_connection(self.source.owner, c)?;
        let (_, package) = snapshot(client, c)?;
        self.consume_package_evidence(client, c, &package)?;
        let expected = &package["result"]["package"];
        let caller = &self.source.owner.configuration.caller_scope;
        if expected["delegation"]["binding_ref"] != caller.binding_ref
            || expected["workspace_uuid"] != caller.podway_workspace_ref
            || expected["session_id"] != caller.podway_session_ref
            || expected["attempt_id"] != caller.podway_attempt_ref
            || expected["placement_id"] != caller.delegated_placement_ref
        {
            return Err(source_integrity());
        }
        let contexts = request["assignment"]["task_body"]["contexts"]
            .as_array()
            .ok_or_else(source_integrity)?;
        if !contexts.iter().any(|v| {
            v["content"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .is_some_and(|v| v == *expected || v["result"]["package"] == *expected)
        }) {
            return Err(error(
                "preimage_stale",
                "accepted_package",
                Effect::NotCalled,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> DelegationIdentity {
        let i: DelegationIdentity = serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-idle.valid.json"
        ))
        .unwrap();

        i.validate().unwrap();
        i
    }
    fn connection(i: &DelegationIdentity) -> PodwayConnection {
        PodwayConnection {
            executable: "/selected/podway".into(),
            executable_sha256: hash(b"binary"),
            manifest_digest: hash(b"manifest"),
            worktree: "/selected/worktree".into(),
            workspace: Uuid::now_v7(),
            session: Uuid::now_v7(),
            attempt: Uuid::now_v7(),
            placement: "review".into(),
            adapter_id: i.adapter_id.clone(),
            adapter_version: i.adapter_version.clone(),
            permitted_items: BTreeSet::from(["summary".into()]),
        }
    }
    fn scene() -> (DelegationIdentity, PodwayConnection, Value, Value) {
        let mut i = identity();
        i.binding_ref = "binding".into();
        i.caller_scope.binding_ref = "binding".into();
        let c = connection(&i);
        i.caller_scope.podway_workspace_ref = c.workspace.to_string();
        i.caller_scope.podway_session_ref = c.session.to_string();
        i.caller_scope.podway_attempt_ref = c.attempt.to_string();
        i.caller_scope.delegated_placement_ref = c.placement.clone();
        let body = json!({"schema":"podway.delegation-work-package/v1","workspace_uuid":c.workspace,"session_id":c.session,"attempt_id":c.attempt,
            "procedure_digest":hash(b"procedure"),"placement_id":c.placement,"delegation":{"binding_ref":i.binding_ref,"role_ref":i.role_ref,
            "role_snapshot_digest":i.role_snapshot_digest,"max_rounds":32,"after_accept":"retain"},
            "action":{"title":"Review","intent":"Inspect the result.","items":[{"id":"summary","type":"text","prompt":"Summary","required":true}]},"goal":null,"evidence":[]});
        let p = json!({"workspace":{"uuid":c.workspace},"result":{"schema":"podway.delegation-package-result/v1","package_digest":package_digest(&body).unwrap(),
            "package":body,"workspace_uuid":c.workspace,"session_id":c.session,"attempt_id":c.attempt,"session_revision":u64::MAX}});
        let o = json!({"workspace":{"uuid":c.workspace},"result":{"schema":"podway.observation-result/v5","status":{"session":{"id":c.session,"revision":u64::MAX,"lifecycle":"running"},
            "current":{"node":{"graph_node_id":c.placement},"attempt":{"attempt_id":c.attempt}}},"active_items":[{"item_id":"summary","revision":u64::MAX}],"active_items_total":1,"active_items_truncated":false}});
        (i, c, o, p)
    }
    #[test]
    fn selected_acceptance_has_exact_thirteen_fields_and_full_u64_literal() {
        let (i, c, o, p) = scene();
        validate_snapshot(&c, &o, &p).unwrap();
        let bytes = accept_input(&c, &p, &i, "original-key", "Accepted the evidence.").unwrap();
        let value: Value = decode(&bytes, 32768).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 13);
        assert_eq!(value["execution"].as_object().unwrap().len(), 13);
        assert_eq!(value["session_revision"].as_u64(), Some(u64::MAX));
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("18446744073709551615")
        );
        for key in [
            "accepted_assignments",
            "unresolved_assignments",
            "complete_result",
            "disposition_proof",
            "accepted_items_digest",
        ] {
            assert!(value.get(key).is_none());
        }
    }
    #[test]
    fn every_podway_preimage_and_execution_projection_drift_is_distinct() {
        let (i, c, o, p) = scene();
        let baseline = preimage(&o, &p, Some(projection(&i).unwrap()));
        for path in [
            "/workspace/uuid",
            "/result/status/session/id",
            "/result/status/session/revision",
            "/result/status/current/node/graph_node_id",
            "/result/status/current/attempt/attempt_id",
            "/result/active_items/0/revision",
        ] {
            let mut changed = o.clone();
            *changed.pointer_mut(path).unwrap() = json!("changed");
            assert_ne!(
                preimage(&changed, &p, Some(projection(&i).unwrap())),
                baseline,
                "{path}"
            );
            assert!(validate_snapshot(&c, &changed, &p).is_err());
        }
        for key in [
            "workspace_uuid",
            "session_id",
            "session_revision",
            "attempt_id",
            "package_digest",
        ] {
            let mut changed = p.clone();
            changed["result"][key] = json!("changed");
            assert!(validate_snapshot(&c, &o, &changed).is_err(), "{key}");
        }
        for key in execution(&i).unwrap().as_object().unwrap().keys() {
            let mut projected = projection(&i).unwrap();
            projected["execution"][key] = json!("changed");
            assert_ne!(preimage(&o, &p, Some(projected)), baseline, "{key}");
        }
        for key in ["rounds_used", "result_digest", "worker_disposition"] {
            let mut projected = projection(&i).unwrap();
            projected[key] = json!("changed");
            assert_ne!(preimage(&o, &p, Some(projected)), baseline);
        }
    }
    #[test]
    fn valid_unrelated_proof_revisions_do_not_stale_the_projection() {
        let i = identity();
        let mut changed = i.clone();
        changed.disposition_proof.membership_revision += 1;
        changed.disposition_proof.workload_revision += 1;
        changed.validate().unwrap();
        assert_eq!(projection(&i).unwrap(), projection(&changed).unwrap());
    }
    #[test]
    fn selection_does_not_store_the_complete_result_and_accepts_twenty_thousand_changes() {
        let mut i = identity();
        i.complete_result.changes = (0..20_000)
            .map(|n| crate::delegation_identity::ChangeRef {
                kind: "add".into(),
                path: format!("{}-{n:05}", "long-name".repeat(15)),
                old_path: None,
                before: None,
                after: Some(BytesRef::from_bytes(b"x")),
                before_mode: None,
                after_mode: Some(0o100644),
            })
            .collect();
        i.result_digest = i.complete_result.digest().unwrap();
        i.disposition_proof.observed_after_result_digest = i.result_digest.clone();
        i.validate().unwrap();
        let body = encode(&i.complete_result).unwrap();
        assert!(body.len() > MAX_INPUT);
        let selection = Selection::Result {
            scope: i.scope_ref.clone(),
            member: Uuid::now_v7(),
            task: Uuid::now_v7(),
            result_digest: i.result_digest.clone(),
            accepted_task_digest: i.accepted_task_digest.clone(),
            projection: projection(&i).unwrap(),
        };
        let small = encode(&selection).unwrap();
        assert!(small.len() < 8192);
        assert!(
            !String::from_utf8(small.clone())
                .unwrap()
                .contains("complete_result")
        );
        let _: Selection = decode(&small, MAX_INPUT).unwrap();
        let mut offset = 0;
        let mut rebuilt: Vec<u8> = Vec::new();
        while offset < body.len() as u64 {
            let (text, encoding, end) = page(&body, offset).unwrap();
            assert_eq!(encoding, "utf8");
            rebuilt.extend(text.as_bytes());
            assert!(end > offset);
            offset = end;
        }
        assert_eq!(rebuilt, body);
    }
    #[test]
    fn original_text_above_control_bound_and_binary_pages_keep_source_bytes() {
        let raw = format!("{}한글🙂\r\n", "x".repeat(MAX_INPUT + 23)).into_bytes();
        assert!(raw.len() > MAX_INPUT);
        let reference = BytesRef::from_bytes(&raw);
        verify_bytes(&reference, &raw).unwrap();
        let mut offset = 0;
        let mut rebuilt: Vec<u8> = Vec::new();
        while offset < raw.len() as u64 {
            let (text, encoding, end) = page(&raw, offset).unwrap();
            assert_eq!(encoding, "utf8");
            rebuilt.extend(text.as_bytes());
            offset = end;
        }
        assert_eq!(rebuilt, raw);
        for (input, offset, expected) in [
            ("한글🙂".as_bytes(), 1, false),
            ("한글🙂".as_bytes(), 3, true),
            (b"abc".as_slice(), 4, false),
            (b"abc".as_slice(), 3, true),
        ] {
            assert_eq!(page(input, offset).is_ok(), expected);
        }
        let bytes = [0xff, 0, 1];
        let (encoded, encoding, end) = page(&bytes, 0).unwrap();
        assert_eq!((encoding, end), ("base64", 3));
        use base64::Engine as _;
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            bytes
        );
        assert!(verify_bytes(&reference, b"corruption").is_err());
    }
    #[test]
    fn record_variants_follow_declarations_and_retained_check_evidence() {
        let i = identity();
        let bytes = b"actual check output".to_vec();
        let digest = hash(&bytes);
        let material = Materialized {
            identity: i.clone(),
            views: BTreeMap::from([("check".into(), bytes)]),
            accepted_contexts: Vec::new(),
        };
        let good = [
            (
                RecordValue::Confirm { value: true },
                json!({"type":"confirm"}),
            ),
            (
                RecordValue::Text {
                    value: "abc".into(),
                },
                json!({"type":"text","min_length":3,"max_length":3}),
            ),
            (
                RecordValue::Choice { value: "a".into() },
                json!({"type":"choice","choices":["a"]}),
            ),
            (
                RecordValue::Integer {
                    value: i64::MIN.to_string(),
                },
                json!({"type":"integer","minimum":i64::MIN,"maximum":i64::MAX}),
            ),
            (
                RecordValue::List {
                    value: vec!["a".into()],
                },
                json!({"type":"list","min_items":1,"max_items":1,"unique":true}),
            ),
            (
                RecordValue::CheckResult {
                    operation_id: "check".into(),
                    operation_digest: hash(b"operation"),
                    input_basis: InputBasis {
                        descriptor: "accepted task".into(),
                        digest: i.accepted_task_digest.clone(),
                    },
                    executor: Executor {
                        name: "checker".into(),
                        version: "1".into(),
                    },
                    outcome: CheckOutcome::Pass,
                    summary: "Passed".into(),
                    output_digest: digest.clone(),
                },
                json!({"type":"check_result","operation_id":"check","operation_digest":hash(b"operation"),"accepted_outcomes":["pass"]}),
            ),
        ];
        for (value, declaration) in good {
            let result = declared_record(&value, &declaration, None, &material).unwrap();
            assert_eq!(result["type"], declaration["type"]);
            let mut bad = declaration.clone();
            bad["type"] = json!("foreign");
            assert!(declared_record(&value, &bad, None, &material).is_err());
            if matches!(value, RecordValue::CheckResult { .. }) {
                for field in ["operation_id", "operation_digest"] {
                    let mut bad = declaration.clone();
                    bad[field] = json!("foreign");
                    assert!(declared_record(&value, &bad, None, &material).is_err());
                }
                assert!(
                    declared_record(
                        &value,
                        &declaration,
                        None,
                        &Materialized {
                            identity: i.clone(),
                            views: BTreeMap::new(),
                            accepted_contexts: Vec::new()
                        }
                    )
                    .is_err()
                );
            }
        }
        let artifact = RecordValue::Artifact {
            artifact_ref: "owned-source".into(),
        };
        let reference = json!({"type":"artifact","reference":"dolgorae:task:owned","digest":digest,"size_bytes":19,"media_type":"text/plain"});
        let result = declared_record(
            &artifact,
            &json!({"type":"artifact","allowed_media_types":["text/plain"]}),
            Some(reference),
            &material,
        )
        .unwrap();
        assert!(result.get("path").is_none());
        assert!(declared_record(&artifact, &json!({"type":"artifact"}), None, &material).is_err());
        for (value, declaration) in [
            (
                RecordValue::Integer { value: "-1".into() },
                json!({"type":"integer","minimum":0}),
            ),
            (
                RecordValue::Text {
                    value: "a\nb".into(),
                },
                json!({"type":"text","multiline":false}),
            ),
            (
                RecordValue::Choice { value: "b".into() },
                json!({"type":"choice","choices":["a"]}),
            ),
            (
                RecordValue::List {
                    value: vec!["a".into(), "a".into()],
                },
                json!({"type":"list","unique":true}),
            ),
        ] {
            assert!(declared_record(&value, &declaration, None, &material).is_err());
        }
    }
    #[test]
    fn context_selection_retains_fences_without_package_or_item_values() {
        let (i, _, mut o, mut p) = scene();
        o["result"]["active_items"][0]["value"] = json!("x".repeat(MAX_INPUT));
        p["result"]["package"]["action"]["description"] = json!("y".repeat(MAX_INPUT));
        let selected = Selection::Context {
            preimage: preimage(&o, &p, None),
        };
        let bytes = encode(&selected).unwrap();
        assert!(bytes.len() < 4096);
        let _: Selection = decode(&bytes, MAX_INPUT).unwrap();
        let material = Materialized {
            identity: i,
            views: BTreeMap::new(),
            accepted_contexts: vec![encode(&p["result"]["package"]).unwrap()],
        };
        assert!(
            verify_original_package(&material, &p).is_err(),
            "invalid package size/digest cannot be accepted merely because a context exists"
        );
    }
    #[test]
    fn native_projection_preserves_unmodified_h1_identity_and_delegated_checks_remain_strict() {
        let i: DelegationIdentity = serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-idle.valid.json"
        ))
        .unwrap();
        i.validate().unwrap();
        assert_eq!(i.binding_ref, "binding:fixture");
        let projected = projection(&i).unwrap();
        assert_eq!(projected["execution"]["binding_ref"], i.binding_ref);
        assert!(podway_execution(&i).is_err());
    }
    #[test]
    fn selected_artifact_read_forms_and_complete_large_list_use_their_own_domains() {
        for (form, kind) in [("path", "local_path"), ("reference", "external_reference")] {
            let data = json!({"location_type":form,"location":"retained-source","sha256_digest":hash(b"source"),"size_bytes":6,"media_type":"text/plain"});
            let converted = recorded_value("artifact", data).unwrap();
            assert_eq!(
                converted,
                json!({"kind":"artifact","location_kind":kind,"location":"retained-source","digest":hash(b"source"),"size_bytes":6,"media_type":"text/plain"})
            );
        }
        let list = vec!["한".repeat(8192); 122];
        let value = recorded_value("list", json!(list)).unwrap();
        assert!(encode(&value).unwrap().len() > MAX_INPUT);
        assert!(recorded_value("list", json!(vec!["한".repeat(8192); 123])).is_err());
        assert!(recorded_value("artifact",json!({"location_kind":"external_reference","location":"x","digest":hash(b"x"),"size_bytes":1,"media_type":"text/plain"})).is_err());
    }
    #[test]
    fn native_semantic_mappings_keep_every_actionable_public_fact() {
        let corpus: Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/successor-facade-v5-methods.valid.json"
        ))
        .unwrap();
        let mut seen = BTreeSet::new();
        for case in corpus["cases"].as_array().unwrap() {
            let operation = case["operation"].as_str().unwrap();
            if !crate::specialist_facade::EXTERNAL.contains(&operation)
                && !crate::specialist_facade::MANAGED.contains(&operation)
            {
                continue;
            }
            seen.insert(operation.to_owned());
            let public = &case["response"];
            let presented = semantic_presentation(public);
            assert_eq!(presented["operation"], operation);
            assert_eq!(presented["ok"], true);
            for field in [
                "candidate",
                "member",
                "engagement",
                "lookup",
                "receipt",
                "operation_ref",
                "wait_timed_out",
            ] {
                if let Some(expected) = public.get(field) {
                    assert_eq!(&presented["facts"][field], expected, "{operation}/{field}");
                }
            }
            if let Some(task) = public.get("task") {
                let mut expected = task.clone();
                expected.as_object_mut().unwrap().remove("complete_result");
                assert_eq!(presented["facts"]["task"], expected, "{operation}/task");
            }
            for managed in [false, true] {
                native_content(&presented, managed).unwrap();
            }
            let failure = semantic_presentation(&case["failure"]);
            assert_eq!(failure["semantic"]["error"], case["failure"]["error"]);
            assert_eq!(
                failure["semantic"]["classification"],
                case["failure"]["error"]["classification"]
            );
            assert_eq!(
                failure["semantic"]["effect"],
                case["failure"]["error"]["effect"]
            );
            assert!(native_failed(&failure));
            assert_eq!(
                presentation_failure(&presented)["semantic"],
                presented["semantic"]
            );
        }
        assert_eq!(seen.len(), 20);
        // Frozen candidate and the actual hire request consumer require these
        // exact authoritative next-step fields, including the installed state.
        let candidate = &corpus["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["operation"] == "install_provisioning")
            .unwrap()["response"];
        let native = semantic_presentation(candidate);
        let hire = json!({"schema":"dolgorae.external-specialist-request/v5","operation":"hire_external_specialist","scope_kind":"external_engagement","scope_ref":Uuid::now_v7(),"provisioning_ref":native["facts"]["candidate"]["provisioning_ref"],"role_ref":"reviewer","agent_configuration_digest":native["facts"]["candidate"]["agent_configuration_digest"],"idempotency_key":"native-journey","expected_revision":null});
        crate::host_invocation::validate_public_request(&hire).unwrap();
        assert_eq!(native["facts"]["candidate"]["installed"], true);
        assert_eq!(
            hire["agent_configuration_digest"],
            candidate["candidate"]["agent_configuration_digest"]
        );
    }
    #[test]
    fn native_pages_use_original_blocks_through_complete_wire_and_history_bounds() {
        for raw in [
            "한글🙂\\\"\n\t\r".repeat(180),
            "x".repeat(1023) + "🙂",
            "".into(),
        ] {
            let bytes = raw.as_bytes();
            let (original, encoding, end) = page(bytes, 0).unwrap();
            assert!(end <= 1024);
            let value = json!({"schema":"dolgorae.consumer-control-response/v1","action":"inspect","ok":true,"request_id":null,"data":{"kind":"page","source_digest":hash(bytes),"source_length":bytes.len(),"offset":0,"returned_length":end,"encoding":encoding,"continuation":null,"text":original}});
            for managed in [false, true] {
                let content = native_content(&value, managed).unwrap();
                assert_eq!(content.as_array().unwrap().len(), 2);
                let header = content[0]["text"].as_str().unwrap();
                assert!(header.is_ascii());
                assert!(
                    serde_json::from_str::<Value>(header).unwrap()["data"]
                        .get("text")
                        .is_none()
                );
                assert_eq!(content[1]["text"], original);
                let native = if managed {
                    json!({"id":u64::MAX,"result":{"success":true,"contentItems":content}})
                } else {
                    json!({"jsonrpc":"2.0","id":"native-correlation","result":{"content":content,"isError":false}})
                };
                let wire = encode(&native).unwrap();
                assert!(wire.len() < 8 * 1024 * 1024);
                let decoded: Value = serde_json::from_slice(&wire).unwrap();
                let delivered = if managed {
                    &decoded["result"]["contentItems"]
                } else {
                    &decoded["result"]["content"]
                };
                assert_eq!(
                    delivered[1]["text"].as_str().unwrap().as_bytes(),
                    &bytes[..end as usize]
                );
                // Exact selected source uses 1.2 allowance and ceil(bytes/4).
                // Both the MCP and common FunctionCallOutput history steps see
                // the header and original block, not raw event/rollout output.
                let history_bytes = delivered
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["text"].as_str().unwrap().len())
                    .sum::<usize>();
                assert!(history_bytes < 10_000 * 12 / 10);
                assert!(history_bytes.div_ceil(4) < 10_000 * 12 / 10);
                let next_http = json!({"input":[{"type":"function_call_output","call_id":"actual-call","output":delivered}]});
                let decoded: Value = serde_json::from_slice(&encode(&next_http).unwrap()).unwrap();
                assert_eq!(decoded["input"][0]["output"][1]["text"], original);
            }
        }
        for effect in ["not_called", "rejected", "unknown", "committed"] {
            let value = json!({"schema":"dolgorae.consumer-control-response/v1","action":"accept","ok":false,"request_id":"original","error":{"code":"post_read_failure","phase":"receipt","effect":effect,"recovery":"inspect_historical_outcome"}});
            let failed = presentation_failure(&value);
            assert_eq!(failed["error"]["effect"], effect);
            native_content(&failed, false).unwrap();
        }
    }
}
