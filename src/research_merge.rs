//! Exact findings merge; ordinary task acceptance owns explicit synthesis.
use super::*;
use crate::delegation_identity::BytesRef as ContentBytesRef;
use rusqlite::OptionalExtension;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const ALGORITHM: &str = "merge-exact-findings-v1";
const KIND: &str = "research_merge";
const MAX_BYTES: u64 = 32 * 1024 * 1024;
fn encode(value: &impl serde::Serialize) -> Result<Vec<u8>, MachineError> {
    crate::operation::canonical(
        &serde_json::to_value(value).map_err(|_| retained_source_invalid())?,
    )
    .map_err(|_| retained_source_invalid())
}
fn capacity() -> MachineError {
    assignment_refusal(
        "ARTIFACT_CAPACITY_EXCEEDED",
        "the complete merged artifact exceeds its checked capacity",
    )
}
fn invalid() -> MachineError {
    MachineError::invalid_argument(
        "merge_group_results",
        "the complete group source set or algorithm is invalid",
    )
}
fn exact_fields(value: &Value, fields: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == fields.len() && fields.iter().all(|field| object.contains_key(*field))
    })
}
fn checked(value: &Value, definition: &str) -> bool {
    let schemas = crate::host_invocation::request_schemas();
    crate::host_invocation::schema_matches(
        value,
        &json!({"$ref":format!("https://dolgorae.local/schema/v014-identities/v1#/$defs/{definition}")}),
        &schemas[0],
        0,
    )
}
fn sorted_sources(sources: &Value) -> Result<Vec<Value>, MachineError> {
    let mut sources = sources
        .as_array()
        .filter(|rows| (1..=64).contains(&rows.len()))
        .ok_or_else(invalid)?
        .clone();
    sources.sort_by(|a, b| a["entry_key"].as_str().cmp(&b["entry_key"].as_str()));
    if sources.iter().any(|row| !checked(row, "merged_source"))
        || sources
            .windows(2)
            .any(|rows| rows[0]["entry_key"] == rows[1]["entry_key"])
    {
        return Err(invalid());
    }
    Ok(sources)
}
type Variants = BTreeMap<Vec<u8>, (Value, BTreeSet<String>)>;
struct Accumulator {
    groups: BTreeMap<String, Variants>,
    encoded_bytes: usize,
}
impl Accumulator {
    fn new(sources: &[Value]) -> Result<Self, MachineError> {
        let empty = json!({"schema":"dolgorae.merged-findings-output/v1","algorithm":ALGORITHM,"groups":[],"coverage":sources});
        Ok(Self {
            groups: BTreeMap::new(),
            encoded_bytes: encode(&empty)?.len(),
        })
    }
    /// Retain unique finding content, not the complete source documents.
    fn add(&mut self, source: &Value, bytes: &[u8]) -> Result<(), MachineError> {
        let canonical = crate::structured_findings::parse(bytes)?;
        let mut output: Value = serde_json::from_slice(&canonical).map_err(|_| invalid())?;
        let key = source["entry_key"].as_str().ok_or_else(invalid)?;
        let link_bytes = encode(&key)?.len();
        for finding in output["findings"]
            .as_array_mut()
            .ok_or_else(invalid)?
            .drain(..)
        {
            let finding_key = finding["finding_key"]
                .as_str()
                .ok_or_else(invalid)?
                .to_owned();
            let canonical = encode(&finding)?;
            let mut additional = 0;
            if let Some(variants) = self.groups.get(&finding_key) {
                if let Some((_, links)) = variants.get(&canonical) {
                    if links.contains(key) {
                        continue;
                    }
                    additional += link_bytes + 1;
                } else {
                    if variants.len() == 64 {
                        return Err(capacity());
                    }
                    additional += encode(&json!({"finding":null,"source_entry_keys":[]}))?.len()
                        - 4
                        + canonical.len()
                        + link_bytes
                        + 1;
                    if variants.len() == 1 {
                        additional -= 1;
                    } // false -> true
                }
            } else {
                additional +=
                    encode(&json!({"finding_key":finding_key,"variants":[],"conflict":false}))?
                        .len()
                        + usize::from(!self.groups.is_empty());
                additional += encode(&json!({"finding":null,"source_entry_keys":[]}))?.len() - 4
                    + canonical.len()
                    + link_bytes;
            }
            if self.encoded_bytes.saturating_add(additional) > MAX_BYTES as usize {
                return Err(capacity());
            }
            self.encoded_bytes += additional;
            self.groups
                .entry(finding_key)
                .or_default()
                .entry(canonical)
                .or_insert_with(|| (finding, BTreeSet::new()))
                .1
                .insert(key.to_owned());
        }
        Ok(())
    }
    fn finish(self, sources: &[Value]) -> Result<Vec<u8>, MachineError> {
        let groups=self.groups.into_iter().map(|(key,variants)| {
            let conflict=variants.len()>1;
            json!({"finding_key":key,"variants":variants.into_values().map(|(finding,links)|json!({"finding":finding,"source_entry_keys":links})).collect::<Vec<_>>(),"conflict":conflict})
        }).collect::<Vec<_>>();
        let output = json!({"schema":"dolgorae.merged-findings-output/v1","algorithm":ALGORITHM,"groups":groups,"coverage":sources});
        if !checked(&output, "merged_output") {
            return Err(invalid());
        }
        let bytes = encode(&output)?;
        if bytes.len() != self.encoded_bytes {
            return Err(retained_source_invalid());
        }
        if bytes.len() as u64 > MAX_BYTES {
            return Err(capacity());
        }
        Ok(bytes)
    }
}
#[cfg(test)]
fn merge_output(
    sources: &[Value],
    findings: &BTreeMap<String, Value>,
) -> Result<Vec<u8>, MachineError> {
    let mut accumulator = Accumulator::new(sources)?;
    for source in sources {
        if source["state"] == "completed" {
            accumulator.add(
                source,
                &encode(
                    findings
                        .get(source["entry_key"].as_str().ok_or_else(invalid)?)
                        .ok_or_else(invalid)?,
                )?,
            )?;
        }
    }
    accumulator.finish(sources)
}
fn identity(
    group_digest: &Value,
    sources: &[Value],
    bytes: &[u8],
) -> Result<(Value, String), MachineError> {
    let identity = json!({"schema":"dolgorae.merged-findings/v1","algorithm":ALGORITHM,"group_assignment_digest":group_digest,"sources":sources,"merged_output":ContentBytesRef::from_bytes(bytes)});
    if !checked(&identity, "merged_findings") {
        return Err(invalid());
    }
    let digest =
        crate::delegation_identity::domain_digest("dolgorae.merged-findings/v1", &identity)?;
    Ok((identity, digest))
}
fn invocation(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    controller: bool,
    intent: &IntentIdentity,
    operation: Uuid,
) -> Result<Value, MachineError> {
    let (origin, call) = if controller {
        if context.is_some() || scope.trusted_invocation.is_some() {
            return Err(retained_source_invalid());
        }
        ("controller", Value::Null)
    } else if let Some(call) = context {
        if scope.trusted_invocation.is_some()
            || intent.lookup.scope_kind.as_deref() != Some("orchestrated_session")
            || intent.lookup.scope_ref.as_deref() != Some(call.session_id.to_string().as_str())
            || call.session_id != call.source_run_id
            || call.idempotency_key != intent.lookup.idempotency_key
        {
            return Err(retained_source_invalid());
        }
        checked_provenance(call.source_turn_id, call.source_tool_call_id)?;
        (
            "managed_native",
            json!({"session_id":call.session_id,"source_run_id":call.source_run_id,"source_turn_id":call.source_turn_id,"source_tool_call_id":call.source_tool_call_id,"idempotency_key":call.idempotency_key}),
        )
    } else {
        if scope.trusted_invocation.is_none()
            || intent.lookup.scope_kind.as_deref() != Some("external_engagement")
        {
            return Err(retained_source_invalid());
        }
        ("external_native", Value::Null)
    };
    Ok(
        json!({"schema":"dolgorae.research-merge-invocation/v1","operation_ref":operation,"accepted_input_digest":intent.digest,"origin":origin,"managed_call":call}),
    )
}

/// The boundary and accepted row, not the current caller, determine provenance.
fn invocation_origin(
    connection: &rusqlite::Connection,
    receipt: &OperationReceipt,
) -> Result<String, MachineError> {
    let cutoff = engagement::merge_origin_cutoff(connection)?;
    let sequence: i64 = connection.query_row(
        "SELECT sequence FROM delegation_operations WHERE operation_ref=?1 AND state='accepted'",
        [&receipt.operation_ref], |row| row.get(0),
    ).map_err(|_| retained_source_invalid())?;
    if sequence <= 0 {
        return Err(retained_source_invalid());
    }
    let merge = receipt
        .accepted_subjects
        .merge_ref
        .as_deref()
        .ok_or_else(retained_source_invalid)?;
    let reference =
        crate::storage_observation::content_reference(connection, KIND, merge, "invocation")?;
    let native = engagement::accepted_native_context(connection, receipt)?;
    if cutoff.is_none_or(|cutoff| sequence <= cutoff) {
        if reference.is_some() || native.is_some() {
            return Err(retained_source_invalid());
        }
        return Ok("controller".into());
    }
    let bytes = crate::storage_observation::read_content(connection, KIND, merge, "invocation")?
        .ok_or_else(retained_source_invalid)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| retained_source_invalid())?;
    if !exact_fields(
        &value,
        &[
            "schema",
            "operation_ref",
            "accepted_input_digest",
            "origin",
            "managed_call",
        ],
    ) || bytes != encode(&value)?
        || value["schema"] != "dolgorae.research-merge-invocation/v1"
        || value["operation_ref"] != receipt.operation_ref
        || value["accepted_input_digest"] != receipt.accepted_input_digest
    {
        return Err(retained_source_invalid());
    }
    let origin = value["origin"]
        .as_str()
        .ok_or_else(retained_source_invalid)?;
    let call = &value["managed_call"];
    match origin {
        "controller" if call.is_null() && native.is_none() => {}
        "external_native"
            if call.is_null()
                && native.is_some()
                && receipt.namespace.scope_kind.as_deref() == Some("external_engagement") => {}
        "managed_native"
            if native.is_none()
                && receipt.namespace.scope_kind.as_deref() == Some("orchestrated_session") =>
        {
            if !exact_fields(
                call,
                &[
                    "session_id",
                    "source_run_id",
                    "source_turn_id",
                    "source_tool_call_id",
                    "idempotency_key",
                ],
            ) || call["session_id"].as_str() != receipt.namespace.scope_ref.as_deref()
                || call["source_run_id"] != call["session_id"]
                || call["idempotency_key"] != receipt.namespace.idempotency_key
            {
                return Err(retained_source_invalid());
            }
            let id = call["session_id"]
                .as_str()
                .ok_or_else(retained_source_invalid)?;
            if Uuid::parse_str(id)
                .ok()
                .is_none_or(|parsed| parsed.get_version_num() != 7 || parsed.to_string() != id)
            {
                return Err(retained_source_invalid());
            }
            checked_provenance(
                call["source_turn_id"]
                    .as_str()
                    .ok_or_else(retained_source_invalid)?,
                call["source_tool_call_id"]
                    .as_str()
                    .ok_or_else(retained_source_invalid)?,
            )
            .map_err(|_| retained_source_invalid())?;
        }
        _ => return Err(retained_source_invalid()),
    }
    Ok(origin.into())
}

pub(super) fn lookup_result_on(
    connection: &rusqlite::Connection,
    receipt: &OperationReceipt,
) -> Result<(ContentBytesRef, String), MachineError> {
    let merge = receipt
        .accepted_subjects
        .merge_ref
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(retained_source_invalid)?;
    let record = record_on(connection, merge)?;
    if receipt.namespace.scope_kind.as_deref() != record["scope_kind"].as_str()
        || receipt.namespace.scope_ref.as_deref() != record["scope_ref"].as_str()
    {
        return Err(retained_source_invalid());
    }
    let bytes = output_on(connection, &record)?;
    Ok((
        ContentBytesRef::from_bytes(&bytes),
        record["digest"]
            .as_str()
            .ok_or_else(retained_source_invalid)?
            .into(),
    ))
}

fn replay_on(
    connection: &rusqlite::Connection,
    scope: &DelegationScope<'_>,
    intent: &IntentIdentity,
    expected_origin: &str,
) -> Result<Option<Value>, MachineError> {
    let Some(receipt) = replay_outcome(
        engagement::intent_lookup(connection, &intent.lookup)?,
        &intent.digest,
        "merge_group_results",
    )?
    else {
        return Ok(None);
    };
    if invocation_origin(connection, &receipt)? != expected_origin
        || engagement::accepted_native_context(connection, &receipt)?.as_ref()
            != scope.trusted_invocation.map(|proof| proof.context())
    {
        return Err(retained_source_invalid());
    }
    let merge = receipt
        .accepted_subjects
        .merge_ref
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(retained_source_invalid)?;
    let record = record_on(connection, merge)?;
    output_on(connection, &record)?;
    Ok(Some(response(&record, &receipt)))
}

pub(super) fn native_lookup(
    scope: &DelegationScope<'_>,
    request: &Value,
) -> Result<IntentLookup, MachineError> {
    let own = research_group::owner(request)?;
    transport_owner(scope, own, None, "lookup_operation")?;
    let proof = scope
        .trusted_invocation
        .ok_or_else(retained_source_invalid)?;
    let normalized = normalized(request)?;
    let intent = merge_intent(scope, own, request, &normalized)?;
    let store = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(
        scope.state_root,
    ))?;
    let snapshot = store
        .connection()
        .unchecked_transaction()
        .map_err(|_| retained_source_invalid())?;
    let (workspace, owner) = store.external_owner_binding(own.scope_ref())?;
    if workspace != scope.view.workspace_id || owner != *scope.controller {
        return Err(owner_refusal_for("lookup_operation"));
    }
    engagement::merge_origin_cutoff(&snapshot)?;
    let observed = engagement::intent_lookup(&snapshot, &intent.lookup)?;
    match &observed {
        IntentLookup::Accepted(_) => {
            replay_on(&snapshot, scope, &intent, "external_native")?
                .ok_or_else(retained_source_invalid)?;
        }
        IntentLookup::Unresolved { operation_ref, .. }
        | IntentLookup::NotAccepted { operation_ref, .. } => {
            let text: Option<String> = snapshot.query_row(
                "SELECT trusted_invocation_json FROM delegation_operations WHERE operation_ref=?1",
                [operation_ref.to_string()], |row| row.get(0),
            ).map_err(|_| retained_source_invalid())?;
            let context: crate::delegation_identity::TrustedInvocationContext =
                serde_json::from_str(&text.ok_or_else(retained_source_invalid)?)
                    .map_err(|_| retained_source_invalid())?;
            if &context != proof.context() {
                return Err(retained_source_invalid());
            }
        }
        IntentLookup::Absent { .. } => {}
    }
    Ok(observed)
}

fn normalized(request: &Value) -> Result<Value, MachineError> {
    if request["algorithm"] != ALGORITHM {
        return Err(invalid());
    }
    let sources = sorted_sources(&request["sources"])?;
    Ok(
        json!({"group_ref":request["group_ref"],"group_assignment_digest":request["group_assignment_digest"],"sources":sources,"algorithm":ALGORITHM}),
    )
}
fn merge_intent(
    scope: &DelegationScope<'_>,
    own: SettlementOwner,
    request: &Value,
    normalized: &Value,
) -> Result<IntentIdentity, MachineError> {
    intent_identity(
        scope,
        "merge_group_results",
        Some((own.scope_kind(), &own.scope_ref().to_string())),
        checked_key(request["idempotency_key"].as_str().ok_or_else(invalid)?)?,
        "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/merge_group_results_normalized_body",
        normalized,
    )
}
pub(crate) fn record(store: &EngagementStore, merge: Uuid) -> Result<Value, MachineError> {
    let snapshot = if store.connection().is_autocommit() {
        Some(
            store
                .connection()
                .unchecked_transaction()
                .map_err(|_| retained_source_invalid())?,
        )
    } else {
        None
    };
    record_on(snapshot.as_ref().map_or(store.connection(), |tx| tx), merge)
}
fn record_on(connection: &rusqlite::Connection, merge: Uuid) -> Result<Value, MachineError> {
    let bytes = crate::storage_observation::read_content(
        connection,
        KIND,
        &merge.to_string(),
        "publication",
    )?
    .ok_or_else(retained_source_invalid)?;
    let record: Value = serde_json::from_slice(&bytes).map_err(|_| retained_source_invalid())?;
    if bytes != encode(&record)?
        || record["schema"] != "dolgorae.research-merge-publication/v1"
        || record["merge_ref"] != merge.to_string()
        || !exact_fields(
            &record,
            &[
                "schema",
                "merge_ref",
                "artifact_id",
                "owner_run",
                "workspace_id",
                "workspace_path",
                "scope_kind",
                "scope_ref",
                "group_ref",
                "identity",
                "digest",
                "normalized",
                "source_results",
            ],
        )
    {
        return Err(retained_source_invalid());
    }
    let identity = &record["identity"];
    if !checked(identity, "merged_findings")
        || record["digest"]
            != crate::delegation_identity::domain_digest("dolgorae.merged-findings/v1", identity)?
    {
        return Err(retained_source_invalid());
    }
    let row:(String,String)=connection.query_row("SELECT receipt_json,mutation_envelope_json FROM delegation_operations WHERE operation_kind='merge_group_results' AND state='accepted' AND json_extract(accepted_subjects_json,'$.merge_ref')=?1",[merge.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|retained_source_invalid())?;
    let receipt: OperationReceipt =
        serde_json::from_str(&row.0).map_err(|_| retained_source_invalid())?;
    receipt.verify_digest()?;
    let envelope: Envelope = serde_json::from_str(&row.1).map_err(|_| retained_source_invalid())?;
    if envelope.request_body.sha256
        != ContentBytesRef::from_bytes(&encode(&record["normalized"])?).sha256
        || envelope.request_body.byte_length != encode(&record["normalized"])?.len() as u64
        || receipt.accepted_subjects.group_ref.as_deref() != record["group_ref"].as_str()
        || record["scope_kind"].as_str() != receipt.namespace.scope_kind.as_deref()
        || record["scope_ref"].as_str() != receipt.namespace.scope_ref.as_deref()
        || record["workspace_id"] != receipt.namespace.workspace_ref
        || record["owner_run"].as_str() != receipt.accepted_subjects.run_ref.as_deref()
        || record["normalized"]["sources"] != identity["sources"]
        || record["normalized"]["group_assignment_digest"] != identity["group_assignment_digest"]
        || record["normalized"]["algorithm"] != ALGORITHM
        || record["normalized"]["group_ref"] != record["group_ref"]
    {
        return Err(retained_source_invalid());
    }
    if receipt.namespace.operation_kind != "merge_group_results"
        || receipt.accepted_subjects.merge_ref.as_deref() != Some(merge.to_string().as_str())
        || LookupNamespace::from(&envelope.namespace) != receipt.namespace
        || envelope.request_schema
            != "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/merge_group_results_normalized_body"
        || crate::operation::envelope_digest(&envelope).map_err(|_| retained_source_invalid())?
            != receipt.accepted_input_digest
        || !matches!(engagement::intent_lookup(connection, &receipt.namespace)?, IntentLookup::Accepted(ref original) if original.as_ref() == &receipt)
    {
        return Err(retained_source_invalid());
    }
    invocation_origin(connection, &receipt)?;
    if engagement::result_component_record(
        connection,
        &format!(
            "merge-artifact:v1:{}",
            record["artifact_id"]
                .as_str()
                .ok_or_else(retained_source_invalid)?
        ),
    )?
    .as_deref()
        != Some(merge.to_string().as_str())
        || engagement::result_component_record(
            connection,
            &format!(
                "merge-run:v1:{}:{merge}",
                record["owner_run"]
                    .as_str()
                    .ok_or_else(retained_source_invalid)?
            ),
        )?
        .as_deref()
            != Some(
                identity["merged_output"]["byte_length"]
                    .as_u64()
                    .ok_or_else(retained_source_invalid)?
                    .to_string()
                    .as_str(),
            )
    {
        return Err(retained_source_invalid());
    }
    let sources = sorted_sources(&identity["sources"])?;
    if identity["sources"] != json!(sources) {
        return Err(retained_source_invalid());
    }
    let original = research_group::read_document_on(
        connection,
        record["group_ref"]
            .as_str()
            .ok_or_else(retained_source_invalid)?,
    )?;
    if original["digest"] != identity["group_assignment_digest"]
        || original["manifest"]["scope_kind"] != record["scope_kind"]
        || original["manifest"]["scope_ref"] != record["scope_ref"]
    {
        return Err(retained_source_invalid());
    }
    let references = record["source_results"]
        .as_array()
        .ok_or_else(retained_source_invalid)?;
    if references.len()
        != sources
            .iter()
            .filter(|source| !source["result_digest"].is_null())
            .count()
    {
        return Err(retained_source_invalid());
    }
    for (reference, source) in references.iter().zip(
        sources
            .iter()
            .filter(|source| !source["result_digest"].is_null()),
    ) {
        let (result, publication) = crate::result_component_artifact::retained_source_identity(
            connection,
            source["task_ref"]
                .as_str()
                .ok_or_else(retained_source_invalid)?,
            source["result_digest"]
                .as_str()
                .ok_or_else(retained_source_invalid)?,
            reference
                .get("body_free_identity")
                .map(|value| {
                    serde_json::from_value(value.clone()).map_err(|_| retained_source_invalid())
                })
                .transpose()?
                .as_ref(),
        )?;
        if source != &reference["source"]
            || source["result_digest"] != result.digest()?
            || source["worker_ref"] != result.source.worker_ref
            || source["task_ref"] != result.source.task_ref
            || source["role_snapshot_digest"] != result.source.role_snapshot_digest
            || source["agent_configuration_digest"] != result.source.agent_configuration_digest
            || source["accepted_task_digest"] != result.source.accepted_task_digest
            || reference["publication"] != publication
        {
            return Err(retained_source_invalid());
        }
    }
    Ok(record)
}
fn output(store: &EngagementStore, record: &Value) -> Result<Vec<u8>, MachineError> {
    output_on(store.connection(), record)
}
fn output_on(connection: &rusqlite::Connection, record: &Value) -> Result<Vec<u8>, MachineError> {
    let bytes = crate::storage_observation::read_content(
        connection,
        KIND,
        record["merge_ref"]
            .as_str()
            .ok_or_else(retained_source_invalid)?,
        "merged_output",
    )?
    .ok_or_else(retained_source_invalid)?;
    if serde_json::to_value(ContentBytesRef::from_bytes(&bytes))
        .map_err(|_| retained_source_invalid())?
        != record["identity"]["merged_output"]
        || bytes.len() as u64 > MAX_BYTES
    {
        return Err(retained_source_invalid());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| retained_source_invalid())?;
    if !checked(&value, "merged_output") || value["coverage"] != record["identity"]["sources"] {
        return Err(retained_source_invalid());
    }
    Ok(bytes)
}
fn content(record: &Value) -> Value {
    json!({"content":record["identity"]["merged_output"],"media_type":"application/json","artifact":{"artifact_id":record["artifact_id"],"kind":"ARTIFACT_KIND_FINAL_RESPONSE","visibility":"ARTIFACT_VISIBILITY_CONTROLLER_ONLY"},"artifact_owner":{"workspace":{"absolute_path":record["workspace_path"],"expected_workspace_id":record["workspace_id"]},"run_id":record["owner_run"]}})
}
fn response(record: &Value, receipt: &OperationReceipt) -> Value {
    json!({"schema":"dolgorae.external-specialist-response/v5","operation":"merge_group_results","merged_identity":record["identity"],"merged_digest":record["digest"],"receipt":receipt})
}
fn publication_owner(
    scope: &DelegationScope<'_>,
    own: SettlementOwner,
    sources: &[Value],
    size: u64,
) -> Result<
    (
        Uuid,
        std::path::PathBuf,
        crate::artifact::ArtifactQuotaLease,
    ),
    MachineError,
> {
    let candidates = match own {
        SettlementOwner::Managed { session_id } => vec![
            crate::orchestration::OrchestrationStore::open_observer(scope.state_root)?
                .session(session_id)?
                .root_run_id,
        ],
        SettlementOwner::External { .. } => sources
            .iter()
            .filter_map(|source| source["worker_ref"].as_str())
            .filter_map(|id| Uuid::parse_str(id).ok())
            .collect(),
    };
    let mut seen = BTreeSet::new();
    let mut existing = false;
    for run in candidates {
        if !seen.insert(run) {
            continue;
        }
        let path = crate::run::run_root(scope.state_root, run);
        if !path.exists() {
            continue;
        }
        let manifest =
            crate::run::RunStore::new(crate::workspace::SystemWorkspacePlatform, scope.state_root)
                .load_manifest(run)?;
        if manifest.workspace_id != scope.view.workspace_id {
            return Err(owner_refusal_for("merge_group_results"));
        }
        if let SettlementOwner::External { engagement_id } = own
            && manifest.aggregate_binding.as_ref().is_none_or(|binding| {
                binding.aggregate_id != engagement_id
                    || binding.aggregate_kind
                        != crate::domain::AggregateKind::ExternalSpecialistEngagement
            })
        {
            return Err(owner_refusal_for("merge_group_results"));
        }
        existing = true;
        let directory = path.join("artifacts");
        match crate::workspace::create_directory(&directory, 0o700) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(retained_source_invalid()),
        }
        let lease = crate::artifact::ArtifactQuotaLease::acquire_for_run(&directory, run)?;
        if crate::artifact::logical_artifact_bytes(&directory)?
            .checked_add(size)
            .is_some_and(|bytes| bytes <= crate::artifact::MAX_RUN_ARTIFACT_BYTES)
        {
            return Ok((run, directory, lease));
        }
    }
    if existing {
        Err(capacity())
    } else {
        Err(assignment_refusal(
            "COMPLETE_RESULT_UNAVAILABLE",
            "no proven retained Run can own the merged artifact",
        ))
    }
}

pub(crate) fn merge(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    port: &dyn RunSettlementPort,
    request: &Value,
    controller: bool,
) -> Result<Value, MachineError> {
    boundary(
        "merge_group_results",
        merge_in(scope, context, port, request, controller),
    )
}
fn merge_in(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    port: &dyn RunSettlementPort,
    request: &Value,
    controller: bool,
) -> Result<Value, MachineError> {
    let own = research_group::owner(request)?;
    research_group::authorize_owner(scope, own, context, controller, "merge_group_results")?;
    let normalized = normalized(request)?;
    let sources = sorted_sources(&request["sources"])?;
    let scope_ref = own.scope_ref().to_string();
    let intent = merge_intent(scope, own, request, &normalized)?;
    let operation = Uuid::now_v7();
    let origin = invocation(scope, context, controller, &intent, operation)?;
    let expected_origin = origin["origin"]
        .as_str()
        .ok_or_else(retained_source_invalid)?;
    {
        let observer = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(
            scope.state_root,
        ))?;
        let snapshot = observer
            .connection()
            .unchecked_transaction()
            .map_err(|_| retained_source_invalid())?;
        engagement::merge_origin_cutoff(&snapshot)?;
        if let Some(response) = replay_on(&snapshot, scope, &intent, expected_origin)? {
            return Ok(response);
        }
    }
    let mut store = open_store(scope)?;
    let admission = (|| -> Result<Value, MachineError> {
        let coverage = research_group::collect_in(
            scope,
            context,
            port,
            &json!({"scope_kind":own.scope_kind(),"scope_ref":scope_ref,"group_ref":request["group_ref"],"limit":64}),
            controller,
        )?;
        if coverage["coverage"]["group_assignment_digest"] != request["group_assignment_digest"]
            || coverage["coverage"]["sources"] != json!(sources)
        {
            return Err(assignment_refusal(
                "REVISION_CONFLICT",
                "the supplied source set differs from complete authoritative group coverage",
            ));
        }
        let mut accumulator = Accumulator::new(&sources)?;
        let mut provenance = Vec::new();
        for source in &sources {
            if source["result_digest"].is_null() {
                continue;
            }
            let member = Uuid::parse_str(source["worker_ref"].as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let task = Uuid::parse_str(source["task_ref"].as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let collected =
                research_group::collect_group_task(scope, context, port, own, member, task)?;
            let result: crate::delegation_identity::CompleteResult = serde_json::from_value(
                collected
                    .complete_result
                    .ok_or_else(retained_source_invalid)?,
            )
            .map_err(|_| retained_source_invalid())?;
            if source["result_digest"] != result.digest()? {
                return Err(assignment_refusal(
                    "REVISION_CONFLICT",
                    "the original result changed during merge",
                ));
            }
            if source["state"] == "completed" {
                if result.structured_output_schema.as_deref()
                    != Some(crate::structured_findings::SCHEMA)
                {
                    return Err(assignment_refusal(
                        "FINDINGS_OUTPUT_INVALID",
                        "a completed source has no checked findings output",
                    ));
                }
                let bytes = original_delegation_response(scope.state_root, &result, true)?
                    .ok_or_else(retained_source_invalid)?;
                if result.structured_output.as_ref() != Some(&ContentBytesRef::from_bytes(&bytes)) {
                    return Err(retained_source_invalid());
                }
                accumulator.add(source, &bytes)?;
            }
            let publication = crate::result_component_artifact::source_identity(
                scope.state_root,
                scope.view,
                &result,
            )?;
            let mut reference = json!({"source":source,"publication":publication});
            if reference["publication"]["source_manifest"].is_null() {
                reference["body_free_identity"] = json!(result);
            }
            provenance.push(reference);
        }
        let merged = accumulator.finish(&sources)?;
        let (identity, digest) = identity(&request["group_assignment_digest"], &sources, &merged)?;
        let (owner_run, directory, quota) =
            publication_owner(scope, own, &sources, merged.len() as u64)?;
        let merge = Uuid::now_v7();
        let artifact = Uuid::now_v7();
        let now = now_ms()?;
        let publication = json!({"schema":"dolgorae.research-merge-publication/v1","merge_ref":merge,"artifact_id":artifact,"owner_run":owner_run,"workspace_id":scope.view.workspace_id,"workspace_path":scope.view.canonical_path.to_path_buf()?,"scope_kind":own.scope_kind(),"scope_ref":scope_ref,"group_ref":request["group_ref"],"identity":identity,"digest":digest,"normalized":normalized,"source_results":provenance});
        let receipt = store.transact(|tx| {
            if let Some(response) = replay_on(tx, scope, &intent, expected_origin)? {
                return serde_json::from_value(response["receipt"].clone())
                    .map_err(|_| retained_source_invalid());
            }
            let revision = engagement::scope_membership_revision(
                tx,
                &scope.view.workspace_id,
                own.scope_kind(),
                &scope_ref,
            )?;
            if request["expected_revision"]
                .as_u64()
                .is_some_and(|expected| expected != revision)
            {
                return Err(assignment_refusal(
                    "REVISION_CONFLICT",
                    "the merge membership revision moved",
                ));
            }
            research_group::require_active_group_owner(tx, scope, own)?;
            engagement::establish_merge_origin_cutoff(tx)?;
            let preparation = engagement::intent_prepare(
                tx,
                IntentAdmission {
                    operation_ref: operation,
                    namespace: &intent.lookup,
                    mutation_envelope_json: &intent.json,
                    mutation_envelope_digest: &intent.digest,
                    trusted_invocation: intent.trusted_invocation.as_ref(),
                    expected_revision: request["expected_revision"].as_u64(),
                    now_ms: now,
                },
            )?;
            match preparation {
                IntentPreparation::Accepted(receipt) => return Ok(*receipt),
                IntentPreparation::Unresolved { operation_ref } => {
                    return Err(unresolved_refusal("merge_group_results", operation_ref));
                }
                IntentPreparation::Created => {}
            }

            crate::storage_observation::put_content(
                tx,
                KIND,
                &merge.to_string(),
                "publication",
                &encode(&publication)?,
            )?;
            crate::storage_observation::put_content(
                tx,
                KIND,
                &merge.to_string(),
                "merged_output",
                &merged,
            )?;
            crate::storage_observation::put_content(
                tx,
                KIND,
                &merge.to_string(),
                "invocation",
                &encode(&origin)?,
            )?;
            engagement::retain_result_component_record(
                tx,
                &format!("merge-artifact:v1:{artifact}"),
                &merge.to_string(),
            )?;
            engagement::retain_result_component_record(
                tx,
                &format!("merge-run:v1:{owner_run}:{merge}"),
                &merged.len().to_string(),
            )?;
            let receipt = build_receipt(
                &intent.namespace,
                operation,
                &intent.digest,
                AcceptedSubjects {
                    run_ref: Some(owner_run.to_string()),
                    group_ref: request["group_ref"].as_str().map(str::to_owned),
                    merge_ref: Some(merge.to_string()),
                    ..Default::default()
                },
                revision,
            )?;
            engagement::intent_accept(tx, operation, &receipt, now)?;
            lookup_result_on(tx, &receipt)?;
            quota.verify_for(&directory)?;
            Ok(receipt)
        })?;
        let winning = Uuid::parse_str(
            receipt
                .accepted_subjects
                .merge_ref
                .as_deref()
                .ok_or_else(retained_source_invalid)?,
        )
        .map_err(|_| retained_source_invalid())?;
        let snapshot = store
            .connection()
            .unchecked_transaction()
            .map_err(|_| retained_source_invalid())?;
        let record = record_on(&snapshot, winning)?;
        output_on(&snapshot, &record)?;
        Ok(response(&record, &receipt))
    })();
    match admission {
        Ok(response) => Ok(response),
        Err(error) => {
            let snapshot = store
                .connection()
                .unchecked_transaction()
                .map_err(|_| retained_source_invalid())?;
            if let Some(response) = replay_on(&snapshot, scope, &intent, expected_origin)? {
                return Ok(response);
            }
            Err(error)
        }
    }
}

pub(crate) fn public_merged(
    scope: &DelegationScope<'_>,
    merge: Uuid,
) -> Result<Value, MachineError> {
    let store = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(
        scope.state_root,
    ))?;
    let _snapshot = store
        .connection()
        .unchecked_transaction()
        .map_err(|_| retained_source_invalid())?;
    let record = record(&store, merge)?;
    if record["workspace_id"] != scope.view.workspace_id
        || record["scope_kind"]
            .as_str()
            .zip(record["scope_ref"].as_str())
            .is_none()
    {
        return Err(owner_refusal_for("merge_group_results"));
    }
    Ok(json!({"merge_id":merge,"merged_output":content(&record)}))
}
pub(crate) fn logical_bytes(state_root: &Path, run: Uuid) -> Result<u64, MachineError> {
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state_root))?;
    engagement::result_component_records(store.connection(), &format!("merge-run:v1:{run}:"))?
        .into_iter()
        .try_fold(0u64, |total, raw| {
            total
                .checked_add(raw.parse::<u64>().map_err(|_| retained_source_invalid())?)
                .ok_or_else(retained_source_invalid)
        })
}
pub(crate) fn read_artifact(
    state_root: &Path,
    run: Uuid,
    id: Uuid,
    carrier: Option<&crate::controller::CredentialCarrier>,
) -> Result<Option<(crate::artifact::ArtifactReference, Vec<u8>)>, MachineError> {
    let path = EngagementStore::workspace_database_path(state_root);
    if !path.exists() {
        return Ok(None);
    }
    let store = EngagementStore::open_read_only(&path)?;
    let Some(merge) = engagement::result_component_record(
        store.connection(),
        &format!("merge-artifact:v1:{id}"),
    )?
    else {
        return Ok(None);
    };
    let merge = Uuid::parse_str(&merge).map_err(|_| retained_source_invalid())?;
    let _snapshot = store
        .connection()
        .unchecked_transaction()
        .map_err(|_| retained_source_invalid())?;
    let record = record(&store, merge)?;
    if record["artifact_id"] != id.to_string() || record["owner_run"] != run.to_string() {
        return Err(MachineError::new(
            "ARTIFACT_NOT_FOUND",
            "artifact is not owned by this Run",
            false,
            json!({"run_id":run,"artifact_id":id}),
        ));
    }
    let carrier = carrier.ok_or_else(|| {
        MachineError::new(
            "CONTROLLER_REQUIRED",
            "the merged artifact requires its aggregate Controller",
            false,
            json!({}),
        )
    })?;
    let authorize = || {
        research_group::public_owner_binding(
            state_root,
            record["workspace_id"]
                .as_str()
                .ok_or_else(retained_source_invalid)?,
            carrier,
            record["scope_kind"]
                .as_str()
                .ok_or_else(retained_source_invalid)?,
            Uuid::parse_str(
                record["scope_ref"]
                    .as_str()
                    .ok_or_else(retained_source_invalid)?,
            )
            .map_err(|_| retained_source_invalid())?,
            None,
        )
        .map(|_| ())
    };
    authorize()?;
    let bytes = output(&store, &record)?;
    authorize()?;
    Ok(Some((
        crate::artifact::ArtifactReference {
            artifact_id: id.to_string(),
            created_at: String::new(),
            interaction_request_id: None,
            kind: crate::artifact::ArtifactKind::FinalResponse,
            visibility: crate::artifact::ArtifactVisibility::ControllerOnly,
            media_type: "application/json".into(),
            byte_length: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        },
        bytes,
    )))
}

pub(super) fn accepted_key(
    scope: &DelegationScope<'_>,
    operation: &str,
    key: &str,
    own: SettlementOwner,
) -> Result<bool, MachineError> {
    let namespace = Namespace::delegation(
        &scope.controller.identity,
        &scope.view.workspace_id,
        Some((own.scope_kind(), &own.scope_ref().to_string())),
        operation,
        checked_key(key)?,
    )
    .map_err(|_| invalid())?;
    let store = open_store(scope)?;
    Ok(matches!(
        engagement::intent_lookup(store.connection(), &(&namespace).into())?,
        IntentLookup::Accepted(_)
    ))
}

/// Only this authenticated owner constructs a synthesis input source.
pub(crate) struct SynthesisSource {
    pub(super) expected: AcceptedTask,
    body: Value,
    normalized: Value,
    publication: Value,
    state_root: std::path::PathBuf,
    controller: bool,
}
impl SynthesisSource {
    pub(super) fn schema(&self) -> &'static str {
        if self.expected.task_body_schema == "dolgorae.structured-task/v1" {
            "https://dolgorae.local/schema/v014-identities/v1#/$defs/public_assign_synthesis_normalized_body"
        } else {
            "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/assign_synthesis_normalized_body"
        }
    }
    pub(super) fn normalized_body(&self) -> &Value {
        &self.normalized
    }
    pub(super) fn retain(
        &self,
        connection: &Connection,
        assignment: &crate::caller_scope::CallerAssignment<'_>,
    ) -> Result<(), MachineError> {
        if assignment.workspace_ref != self.publication["workspace_id"]
            || assignment.aggregate_scope_kind != self.publication["scope_kind"]
            || assignment.aggregate_scope_ref != self.publication["scope_ref"]
            || self.normalized["member_ref"] != assignment.member_ref.to_string()
        {
            return Err(retained_source_invalid());
        }
        let binding = json!({"schema":"dolgorae.synthesis-input/v1","merge_ref":self.publication["merge_ref"],"merged_findings_digest":self.publication["digest"],"merged_input":self.publication["identity"]["merged_output"],"normalized":self.normalized,"request_schema":self.schema(),"controller":self.controller,"workspace_id":assignment.workspace_ref,"scope_kind":assignment.aggregate_scope_kind,"scope_ref":assignment.aggregate_scope_ref,"member_ref":assignment.member_ref});
        crate::storage_observation::put_content(
            connection,
            "synthesis_task",
            &assignment.assignment_id.to_string(),
            "input",
            &encode(&binding)?,
        )?;
        Ok(())
    }
    pub(super) fn verify_replay(
        &self,
        store: &EngagementStore,
        receipt: &OperationReceipt,
    ) -> Result<(), MachineError> {
        let task = Uuid::parse_str(
            receipt
                .accepted_subjects
                .task_ref
                .as_deref()
                .ok_or_else(retained_source_invalid)?,
        )
        .map_err(|_| retained_source_invalid())?;
        let binding =
            task_binding(store.connection(), task)?.ok_or_else(retained_source_invalid)?;
        if binding["normalized"] != self.normalized || binding["controller"] != self.controller {
            return Err(caller_scope_mismatch("assign_synthesis"));
        }
        Ok(())
    }
}
impl AssignmentSource for SynthesisSource {
    fn expected(&self) -> &AcceptedTask {
        &self.expected
    }
    fn selection(&self) -> Result<Option<crate::review::ReviewTargetRequest>, MachineError> {
        Ok(None)
    }
    fn synthesis(&self) -> Option<&SynthesisSource> {
        Some(self)
    }
    fn capture(
        &self,
        configuration: &crate::run::AgentConfigurationSnapshot,
        task: &crate::task_request::SpecialistTaskRequest,
        intent: &str,
        target: Option<(&str, &[u8])>,
    ) -> Result<(AcceptedTask, crate::successor_capture::AcceptedTaskContents), MachineError> {
        if self.expected.task_body_schema == "dolgorae.structured-task/v1" {
            let (mut accepted, mut contents) =
                crate::successor_capture::capture_facade_task(configuration, task, intent, target)?;
            accepted.task_body_schema = self.expected.task_body_schema.clone();
            contents.task_body = encode(&self.body)?;
            accepted.task_body = ContentBytesRef::from_bytes(&contents.task_body);
            accepted.contexts =
                serde_json::from_value(self.body["context"].clone()).map_err(|_| invalid())?;
            let store = EngagementStore::open_read_only(
                &EngagementStore::workspace_database_path(&self.state_root),
            )?;
            let merged = output(&store, &self.publication)?;
            contents.contexts.clear();
            let lower_bound = accepted.contexts.iter().try_fold(
                (merged.len() as u64)
                    .checked_add(contents.task_body.len() as u64)
                    .ok_or_else(capacity)?,
                |total, context| {
                    total
                        .checked_add(context.content.byte_length)
                        .ok_or_else(capacity)
                },
            )?;
            if lower_bound > 8 * 1024 * 1024 {
                return Err(assignment_refusal(
                    "ARTIFACT_CAPACITY_EXCEEDED",
                    "the complete synthesis input exceeds its ordinary Turn bound",
                ));
            }
            for context in &accepted.contexts {
                let bytes = if context.kind == "accepted_context"
                    && context.content == ContentBytesRef::from_bytes(&merged)
                    && context.media_type == "application/json"
                {
                    merged.clone()
                } else {
                    retained_context(&store, &self.publication, context)?
                };
                contents.contexts.push((context.identity.clone(), bytes));
            }
            Ok((accepted, contents))
        } else {
            crate::successor_capture::capture_facade_task(configuration, task, intent, target)
        }
    }
    fn verify_capture(
        &self,
        accepted: &AcceptedTask,
        contents: &crate::successor_capture::AcceptedTaskContents,
    ) -> Result<(), MachineError> {
        if accepted != &self.expected || !accepted.images.is_empty() {
            return Err(idempotency_conflict());
        }
        let store = EngagementStore::open_read_only(&EngagementStore::workspace_database_path(
            &self.state_root,
        ))?;
        let bytes = output(&store, &self.publication)?;
        render_input(
            contents.prompt(
                accepted,
                &accepted.execution_intent,
                self.normalized["assignment"]["deadline_seconds"]
                    .as_u64()
                    .ok_or_else(invalid)?,
            )?,
            &self.publication,
            &bytes,
        )?;
        Ok(())
    }
}
fn retained_context(
    store: &EngagementStore,
    publication: &Value,
    context: &crate::delegation_identity::ContextRef,
) -> Result<Vec<u8>, MachineError> {
    if context.kind == "accepted_context" {
        let scope = Uuid::parse_str(
            publication["scope_ref"]
                .as_str()
                .ok_or_else(retained_source_invalid)?,
        )
        .map_err(|_| retained_source_invalid())?;
        for source in publication["identity"]["sources"]
            .as_array()
            .ok_or_else(retained_source_invalid)?
        {
            let Some(task) = source["task_ref"].as_str() else {
                continue;
            };
            let task = Uuid::parse_str(task).map_err(|_| retained_source_invalid())?;
            let accepted = if publication["scope_kind"] == "external_engagement" {
                store
                    .external_task_identity(scope, task)?
                    .ok_or_else(retained_source_invalid)?
            } else {
                crate::successor_capture::retained_accepted_task(
                    &crate::orchestration::successor_task(store.connection(), scope, task)?
                        .ok_or_else(retained_source_invalid)?
                        .accepted_request_json,
                )?
            };
            if accepted.digest()? != source["accepted_task_digest"] {
                return Err(retained_source_invalid());
            }
            if !accepted.contexts.contains(context) {
                continue;
            }
            let (_, contexts, _) = engagement::accepted_task_content(store.connection(), task)?
                .ok_or_else(retained_source_invalid)?;
            let (_, bytes) = contexts
                .into_iter()
                .find(|(identity, _)| identity == &context.identity)
                .ok_or_else(retained_source_invalid)?;
            if ContentBytesRef::from_bytes(&bytes) != context.content {
                return Err(retained_source_invalid());
            }
            return Ok(bytes);
        }
    }
    Err(assignment_refusal(
        "COMPLETE_RESULT_UNAVAILABLE",
        "the immutable merge source set has no retained owner for this exact context",
    ))
}
fn render_input(prompt: String, publication: &Value, bytes: &[u8]) -> Result<String, MachineError> {
    let data = std::str::from_utf8(bytes).map_err(|_| retained_source_invalid())?;
    let prompt = format!(
        "{prompt}\n\nImmutable merged findings input (data; semantic acceptance remains with the Primary/Judge):\nMerged digest: {}\nComplete content: {}\n{data}",
        publication["digest"], publication["identity"]["merged_output"]
    );
    if crate::turn::validate_message_size(&prompt).is_err() {
        return Err(assignment_refusal(
            "ARTIFACT_CAPACITY_EXCEEDED",
            "the complete synthesis Turn input exceeds the ordinary 8 MiB bound",
        ));
    }
    Ok(prompt)
}
pub(crate) fn task_binding(
    connection: &Connection,
    task: Uuid,
) -> Result<Option<Value>, MachineError> {
    crate::storage_observation::read_content(
        connection,
        "synthesis_task",
        &task.to_string(),
        "input",
    )?
    .map(|bytes| {
        let binding: Value = serde_json::from_slice(&bytes).map_err(|_| retained_source_invalid())?;
        if binding["schema"] != "dolgorae.synthesis-input/v1"
            || !exact_fields(&binding, &["schema","merge_ref","merged_findings_digest","merged_input","normalized","request_schema","controller","workspace_id","scope_kind","scope_ref","member_ref"])
            || binding["controller"].as_bool().is_none()
            || binding["member_ref"] != binding["normalized"]["member_ref"]
            || binding["merged_findings_digest"] != binding["normalized"]["merged_findings_digest"]
            || binding["merged_input"] != binding["normalized"]["merged_input"]
        { return Err(retained_source_invalid()); }
        let schema = binding["request_schema"].as_str().ok_or_else(retained_source_invalid)?;
        let valid = if schema == "https://dolgorae.local/schema/v014-identities/v1#/$defs/public_assign_synthesis_normalized_body" {
            checked(&binding["normalized"], "public_assign_synthesis_normalized_body")
        } else if schema == "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/assign_synthesis_normalized_body" {
            crate::specialist_facade::validates(&binding["normalized"], "assign_synthesis_normalized_body")
        } else { false };
        if !valid { return Err(retained_source_invalid()); }
        Ok(binding)
    })
    .transpose()
}
/// Authenticate the retained synthesis input without rendering or recapturing it.
pub(super) fn verify_task_receipt(
    store: &EngagementStore,
    receipt: &OperationReceipt,
    accepted: &AcceptedTask,
) -> Result<Value, MachineError> {
    let binding = verify_receipt_binding(store.connection(), receipt, accepted)?;
    let merge = binding["merge_ref"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(retained_source_invalid)?;
    let publication = record(store, merge)?;
    if publication["workspace_id"] != binding["workspace_id"]
        || publication["scope_kind"] != binding["scope_kind"]
        || publication["scope_ref"] != binding["scope_ref"]
        || publication["digest"] != binding["merged_findings_digest"]
        || publication["identity"]["merged_output"] != binding["merged_input"]
    {
        return Err(retained_source_invalid());
    }
    Ok(binding)
}

fn verify_receipt_binding(
    connection: &Connection,
    receipt: &OperationReceipt,
    accepted: &AcceptedTask,
) -> Result<Value, MachineError> {
    let task = receipt
        .accepted_subjects
        .task_ref
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(retained_source_invalid)?;
    let binding = task_binding(connection, task)?.ok_or_else(retained_source_invalid)?;
    let normalized = &binding["normalized"];
    let member = binding["member_ref"]
        .as_str()
        .ok_or_else(retained_source_invalid)?;
    if receipt.namespace.operation_kind != "assign_synthesis"
        || binding["workspace_id"] != receipt.namespace.workspace_ref
        || binding["scope_kind"].as_str() != receipt.namespace.scope_kind.as_deref()
        || binding["scope_ref"].as_str() != receipt.namespace.scope_ref.as_deref()
        || receipt.accepted_subjects.member_ref.as_deref() != Some(member)
        || receipt.accepted_subjects.run_ref.as_deref() != Some(member)
        || normalized["assignment"]["accepted_task_identity"] != json!(accepted)
        || ContentBytesRef::from_bytes(&encode(&normalized["assignment"]["task_body"])?)
            != accepted.task_body
    {
        return Err(retained_source_invalid());
    }
    let envelope: String = connection.query_row(
        "SELECT mutation_envelope_json FROM delegation_operations WHERE operation_ref=?1 AND state='accepted'",
        [&receipt.operation_ref], |row| row.get(0),
    ).map_err(|_| retained_source_invalid())?;
    let envelope: Envelope =
        serde_json::from_str(&envelope).map_err(|_| retained_source_invalid())?;
    let input = ContentBytesRef::from_bytes(&encode(normalized)?);
    if envelope.request_schema != binding["request_schema"]
        || envelope.request_body.sha256 != input.sha256
        || envelope.request_body.byte_length != input.byte_length
        || envelope.namespace.operation_kind != "assign_synthesis"
    {
        return Err(retained_source_invalid());
    }
    match engagement::intent_lookup(connection, &receipt.namespace)? {
        IntentLookup::Accepted(original) if *original == *receipt => {}
        _ => return Err(retained_source_invalid()),
    }
    let accounting: Option<(String,String,String,String,String)> = connection.query_row(
        "SELECT workspace_ref,aggregate_scope_kind,aggregate_scope_ref,member_ref,accounting_state FROM caller_scope_assignments WHERE assignment_id=?1",
        [task.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).optional().map_err(|_| retained_source_invalid())?;
    if binding["controller"] == accounting.is_some() {
        return Err(retained_source_invalid());
    }
    if let Some((workspace, kind, scope, original_member, state)) = accounting
        && (binding["workspace_id"] != workspace
            || binding["scope_kind"] != kind
            || binding["scope_ref"] != scope
            || member != original_member
            || !matches!(
                state.as_str(),
                "counted_unresolved" | "counted_accepted" | "proved_nondispatch"
            ))
    {
        return Err(retained_source_invalid());
    }
    if binding["controller"] == true
        && engagement::accepted_native_context(connection, receipt)?.is_some()
    {
        return Err(retained_source_invalid());
    }
    Ok(binding)
}

pub(crate) fn task_prompt(
    state_root: &Path,
    task: Uuid,
    prompt: String,
) -> Result<String, MachineError> {
    let store =
        EngagementStore::open_read_only(&EngagementStore::workspace_database_path(state_root))?;
    let Some(binding) = task_binding(store.connection(), task)? else {
        return Ok(prompt);
    };
    let merge = Uuid::parse_str(
        binding["merge_ref"]
            .as_str()
            .ok_or_else(retained_source_invalid)?,
    )
    .map_err(|_| retained_source_invalid())?;
    let publication = record(&store, merge)?;
    if binding["merged_findings_digest"] != publication["digest"]
        || binding["merged_input"] != publication["identity"]["merged_output"]
    {
        return Err(retained_source_invalid());
    }
    render_input(prompt, &publication, &output(&store, &publication)?)
}
pub(crate) fn verify_accounted_task(
    connection: &Connection,
    task: Uuid,
) -> Result<bool, MachineError> {
    let Some(binding) = task_binding(connection, task)? else {
        return Ok(false);
    };
    if binding["controller"] != true {
        return Ok(false);
    }
    let rows = connection.prepare(
        "SELECT receipt_json FROM delegation_operations WHERE operation_kind='assign_synthesis' AND state='accepted' AND json_extract(accepted_subjects_json,'$.task_ref')=?1 LIMIT 2",
    ).map_err(|_| retained_source_invalid())?.query_map([task.to_string()], |row| row.get::<_,String>(0))
        .map_err(|_| retained_source_invalid())?.collect::<Result<Vec<_>,_>>().map_err(|_| retained_source_invalid())?;
    let [text] = rows.as_slice() else {
        return Err(retained_source_invalid());
    };
    let receipt: OperationReceipt =
        serde_json::from_str(text).map_err(|_| retained_source_invalid())?;
    let accepted: AcceptedTask = serde_json::from_value(
        binding["normalized"]["assignment"]["accepted_task_identity"].clone(),
    )
    .map_err(|_| retained_source_invalid())?;
    verify_receipt_binding(connection, &receipt, &accepted)?;
    Ok(true)
}
fn find_merge(
    scope: &DelegationScope<'_>,
    own: SettlementOwner,
    digest: &Value,
    input: &Value,
) -> Result<Value, MachineError> {
    let store = open_store(scope)?;
    let mut query=store.connection().prepare("SELECT json_extract(accepted_subjects_json,'$.merge_ref') FROM delegation_operations WHERE operation_kind='merge_group_results' AND state='accepted' AND workspace_ref=?1 AND scope_kind=?2 AND scope_ref=?3 ORDER BY sequence").map_err(|_|retained_source_invalid())?;
    let ids = query
        .query_map(
            rusqlite::params![
                scope.view.workspace_id,
                own.scope_kind(),
                own.scope_ref().to_string()
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| retained_source_invalid())?;
    for id in ids {
        let publication = record(
            &store,
            Uuid::parse_str(&id.map_err(|_| retained_source_invalid())?)
                .map_err(|_| retained_source_invalid())?,
        )?;
        if publication["digest"] == *digest && publication["identity"]["merged_output"] == *input {
            return Ok(publication);
        }
    }
    Err(MachineError::invalid_argument(
        "merged_input",
        "no retained merge in this aggregate matches the exact input",
    ))
}
pub(crate) fn assign(
    scope: &DelegationScope<'_>,
    context: Option<&ManagedCallContext<'_>>,
    caller: Option<&crate::caller_scope::TrustedCallerScope>,
    port: &dyn RunSettlementPort,
    request: &Value,
    controller: bool,
) -> Result<Value, MachineError> {
    boundary(
        "assign_synthesis",
        (|| {
            let own = research_group::owner(request)?;
            research_group::authorize_owner(scope, own, context, controller, "assign_synthesis")?;
            let member = Uuid::parse_str(request["member_ref"].as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let accepted: AcceptedTask =
                serde_json::from_value(request["assignment"]["accepted_task_identity"].clone())
                    .map_err(|_| invalid())?;
            let normalized = json!({"member_ref":request["member_ref"],"merged_findings_digest":request["merged_findings_digest"],"merged_input":request["merged_input"],"assignment":request["assignment"]});
            let schema = if accepted.task_body_schema == "dolgorae.structured-task/v1" {
                "https://dolgorae.local/schema/v014-identities/v1#/$defs/public_assign_synthesis_normalized_body"
            } else {
                "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/assign_synthesis_normalized_body"
            };
            let key = request["idempotency_key"].as_str().ok_or_else(invalid)?;
            let identity = intent_identity(
                scope,
                "assign_synthesis",
                Some((own.scope_kind(), &own.scope_ref().to_string())),
                checked_key(key)?,
                schema,
                &normalized,
            )?;
            let store = open_store(scope)?;
            if let Some(receipt) = replay(
                &store,
                &identity.lookup,
                &identity.digest,
                "assign_synthesis",
            )? {
                let task = Uuid::parse_str(
                    receipt
                        .accepted_subjects
                        .task_ref
                        .as_deref()
                        .ok_or_else(retained_source_invalid)?,
                )
                .map_err(|_| retained_source_invalid())?;
                let binding =
                    task_binding(store.connection(), task)?.ok_or_else(retained_source_invalid)?;
                if binding["normalized"] != normalized || binding["controller"] != controller {
                    return Err(caller_scope_mismatch("assign_synthesis"));
                }
                if let Some(caller) = caller {
                    verify_replay_scope(&store, &receipt, caller)?;
                }
                let (task, receipt) = match own {
                    SettlementOwner::External { .. } => assigned_observation(&store, &receipt)?,
                    SettlementOwner::Managed { session_id } => managed_assigned_observation(
                        &crate::orchestration::OrchestrationStore::open_observer(scope.state_root)?,
                        session_id,
                        &receipt,
                    )?,
                };
                return Ok(
                    json!({"schema":"dolgorae.external-specialist-response/v5","operation":"assign_synthesis","task":task,"receipt":receipt}),
                );
            }
            let publication = find_merge(
                scope,
                own,
                &request["merged_findings_digest"],
                &request["merged_input"],
            )?;
            let source = SynthesisSource {
                state_root: scope.state_root.to_path_buf(),
                expected: accepted.clone(),
                body: request["assignment"]["task_body"].clone(),
                normalized,
                publication,
                controller,
            };
            let task = crate::successor_capture::execution_task(&accepted, &encode(&source.body)?)?;
            let accounting = caller.map_or(
                AssignmentAccounting::Synthesis(&source),
                AssignmentAccounting::Native,
            );
            let key = request["idempotency_key"].as_str().ok_or_else(invalid)?;
            let deadline = request["assignment"]["deadline_seconds"]
                .as_u64()
                .ok_or_else(invalid)?;
            let (task, receipt) = match own {
                SettlementOwner::External { engagement_id } => assign_external_specialist_task_in(
                    scope,
                    accounting,
                    port,
                    AssignExternalSpecialistTask {
                        engagement_id,
                        member_ref: member,
                        task,
                        execution_intent: &accepted.execution_intent,
                        target: None,
                        deadline_seconds: deadline,
                        idempotency_key: key,
                        expected_revision: request["expected_revision"].as_u64(),
                    },
                    Some(&source),
                    Some(1),
                )?,
                SettlementOwner::Managed { session_id } => {
                    let public = ManagedCallContext {
                        session_id,
                        source_run_id: session_id,
                        source_turn_id: "",
                        source_tool_call_id: "",
                        idempotency_key: key,
                    };
                    let context = context.map_or(public, |native| ManagedCallContext {
                        idempotency_key: key,
                        ..*native
                    });
                    assign_specialist_task_in(
                        scope,
                        &context,
                        accounting,
                        port,
                        AssignSpecialistTask {
                            member_ref: member,
                            task,
                            execution_intent: &accepted.execution_intent,
                            target: None,
                            deadline_seconds: deadline,
                            expected_revision: request["expected_revision"].as_u64(),
                        },
                        Some(&source),
                        Some(1),
                    )?
                }
            };
            Ok(
                json!({"schema":"dolgorae.external-specialist-response/v5","operation":"assign_synthesis","task":task,"receipt":receipt}),
            )
        })(),
    )
}

pub(super) fn replay_synthesis(
    scope: &DelegationScope<'_>,
    store: &EngagementStore,
    own: SettlementOwner,
    member: Uuid,
    key: &str,
    source: &SynthesisSource,
) -> Result<Option<OperationReceipt>, MachineError> {
    let identity = intent_identity(
        scope,
        "assign_synthesis",
        Some((own.scope_kind(), &own.scope_ref().to_string())),
        key,
        source.schema(),
        source.normalized_body(),
    )?;
    let Some(receipt) = replay(
        store,
        &identity.lookup,
        &identity.digest,
        "assign_synthesis",
    )?
    else {
        return Ok(None);
    };
    source.verify_replay(store, &receipt)?;
    if receipt.accepted_subjects.member_ref.as_deref() != Some(member.to_string().as_str()) {
        return Err(retained_source_invalid());
    }
    let task = Uuid::parse_str(
        receipt
            .accepted_subjects
            .task_ref
            .as_deref()
            .ok_or_else(retained_source_invalid)?,
    )
    .map_err(|_| retained_source_invalid())?;
    let accepted = match own {
        SettlementOwner::External { engagement_id } => store
            .external_task_identity(engagement_id, task)?
            .ok_or_else(retained_source_invalid)?,
        SettlementOwner::Managed { session_id } => {
            crate::successor_capture::retained_accepted_task(
                &crate::orchestration::successor_task(store.connection(), session_id, task)?
                    .ok_or_else(retained_source_invalid)?
                    .accepted_request_json,
            )?
        }
    };
    let (bytes, _, _) = engagement::accepted_task_content(store.connection(), task)?
        .ok_or_else(retained_source_invalid)?;
    if accepted != source.expected || ContentBytesRef::from_bytes(&bytes) != accepted.task_body {
        return Err(retained_source_invalid());
    }
    Ok(Some(receipt))
}
pub(crate) fn verify_assignment(
    connection: &Connection,
    task: Uuid,
    member: Uuid,
    accepted: &AcceptedTask,
    body: &[u8],
    deadline: u64,
    expected_sha: &str,
) -> Result<Option<bool>, MachineError> {
    let Some(binding) = task_binding(connection, task)? else {
        return Ok(None);
    };
    let normalized = &binding["normalized"];
    let actual: Value = serde_json::from_slice(body).map_err(|_| retained_source_invalid())?;
    let correct = normalized["member_ref"] == member.to_string()
        && normalized["assignment"]["task_body"] == actual
        && normalized["assignment"]["accepted_task_identity"] == json!(accepted)
        && normalized["assignment"]["deadline_seconds"] == deadline
        && normalized["merged_findings_digest"] == binding["merged_findings_digest"]
        && normalized["merged_input"] == binding["merged_input"]
        && ContentBytesRef::from_bytes(body) == accepted.task_body
        && format!("sha256:{}", sha256_hex(&encode(normalized)?)) == expected_sha;
    Ok(Some(correct))
}
pub(crate) fn validate_managed_origin(
    connection: &Connection,
    origin: &Value,
    member: Uuid,
    session: Uuid,
) -> Result<(), MachineError> {
    if origin["schema"] != "dolgorae.controller-synthesis-origin/v1"
        || origin["member_ref"] != member.to_string()
    {
        return Err(retained_source_invalid());
    }
    let row:Option<(String,String,String,String)>=connection.query_row("SELECT scope_kind,scope_ref,mutation_envelope_json,state FROM delegation_operations WHERE operation_ref=?1 AND operation_kind='assign_synthesis'",[origin["operation_ref"].as_str().ok_or_else(retained_source_invalid)?],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional().map_err(|_|retained_source_invalid())?;
    let Some((kind, scope, envelope, state)) = row else {
        return Err(retained_source_invalid());
    };
    let envelope: Envelope =
        serde_json::from_str(&envelope).map_err(|_| retained_source_invalid())?;
    if kind != "orchestrated_session"
        || scope != session.to_string()
        || !matches!(state.as_str(), "prepared" | "accepted")
        || envelope.namespace.operation_kind != "assign_synthesis"
        || envelope.namespace.scope_ref.as_deref() != Some(scope.as_str())
    {
        return Err(retained_source_invalid());
    }
    Ok(())
}

pub(crate) fn task_projection(
    connection: &Connection,
    task: Uuid,
    bytes: &[u8],
) -> Result<crate::task_request::SpecialistTaskRequest, MachineError> {
    if let Some(binding) = task_binding(connection, task)? {
        let accepted: AcceptedTask = serde_json::from_value(
            binding["normalized"]["assignment"]["accepted_task_identity"].clone(),
        )
        .map_err(|_| retained_source_invalid())?;
        return crate::successor_capture::execution_task(&accepted, bytes);
    }
    serde_json::from_slice(bytes).map_err(|_| retained_source_invalid())
}
pub(crate) fn verify_bound_assignment(
    connection: &Connection,
    task: Uuid,
    member: Uuid,
    accepted: &AcceptedTask,
    body: &[u8],
    deadline: u64,
    digest: &str,
) -> Result<bool, MachineError> {
    if let Some(valid) =
        verify_assignment(connection, task, member, accepted, body, deadline, digest)?
    {
        return Ok(valid);
    }
    let task = serde_json::from_slice(body).map_err(|_| retained_source_invalid())?;
    crate::successor_capture::verify_accepted_assignment(member, &task, accepted, deadline, digest)
}

pub(crate) fn dispatch_key(
    connection: &Connection,
    task: Uuid,
    key: &str,
) -> Result<String, MachineError> {
    Ok(
        if task_binding(connection, task)?
            .is_some_and(|binding| binding["scope_kind"] == "external_engagement")
        {
            format!("synthesis-task:{task}")
        } else {
            key.to_owned()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(key: &str, state: &str) -> Value {
        let digest = format!("sha256:{}", "a".repeat(64));
        json!({"entry_key":key,"worker_ref":format!("member:{key}"),"task_ref":format!("task:{key}"),"role_snapshot_digest":digest,"agent_configuration_digest":digest,"accepted_task_digest":digest,"state":state,"result_digest":if matches!(state,"unknown"|"missing"){Value::Null}else{json!(digest)},"safe_failure_code":null})
    }
    fn findings() -> Value {
        serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-findings.valid.json"
        ))
        .unwrap()
    }
    #[test]
    fn task085_exact_merge_permutation_dedup_conflicts_and_coverage() {
        let mut sources = vec![
            source("b", "completed"),
            source("a", "completed"),
            source("c", "failed"),
            source("d", "cancelled"),
            source("e", "expired"),
            source("f", "unknown"),
            source("g", "missing"),
        ];
        let mut values = BTreeMap::new();
        values.insert("a".into(), findings());
        values.insert("b".into(), findings());
        let sorted = sorted_sources(&json!(sources)).unwrap();
        let first = merge_output(&sorted, &values).unwrap();
        sources.reverse();
        let reversed = sorted_sources(&json!(sources)).unwrap();
        assert_eq!(first, merge_output(&reversed, &values).unwrap());
        let group = json!(format!("sha256:{}", "b".repeat(64)));
        assert_eq!(
            identity(&group, &sorted, &first).unwrap(),
            identity(&group, &reversed, &first).unwrap()
        );
        let output: Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(output["groups"][0]["variants"].as_array().unwrap().len(), 1);
        assert_eq!(
            output["groups"][0]["variants"][0]["source_entry_keys"],
            json!(["a", "b"])
        );
        assert_eq!(output["coverage"], json!(sorted));
        assert_eq!(output["groups"][0]["conflict"], false);
        values.get_mut("b").unwrap()["findings"][0]["claim"] = json!("An inequivalent claim.");
        let conflicting: Value =
            serde_json::from_slice(&merge_output(&sorted, &values).unwrap()).unwrap();
        assert_eq!(conflicting["groups"][0]["conflict"], true);
        assert_eq!(
            conflicting["groups"][0]["variants"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        values.get_mut("b").unwrap()["findings"][0] = findings()["findings"][0].clone();
        values.get_mut("b").unwrap()["findings"][0]["limitations"] =
            json!(["A different limitation."]);
        let differing: Value =
            serde_json::from_slice(&merge_output(&sorted, &values).unwrap()).unwrap();
        assert_eq!(
            differing["groups"][0]["variants"].as_array().unwrap().len(),
            2,
            "limitations are part of exact equivalence"
        );
    }
    #[test]
    fn task085_too_many_same_key_variants_refuse_without_dropping_sources() {
        let sources = vec![source("a", "completed"), source("b", "completed")];
        let mut values = BTreeMap::new();
        for (key, start, count) in [("a", 0, 64), ("b", 64, 1)] {
            let mut value = findings();
            let mut entries = Vec::new();
            for index in start..start + count {
                let mut finding = value["findings"][0].clone();
                finding["claim"] = json!(format!("Claim {index}"));
                entries.push(finding);
            }
            value["findings"] = json!(entries);
            values.insert(key.into(), value);
        }
        assert_eq!(
            merge_output(&sources, &values).unwrap_err().code,
            "ARTIFACT_CAPACITY_EXCEEDED"
        );
        assert_eq!(sources.len(), 2);
        assert_eq!(values["a"]["findings"].as_array().unwrap().len(), 64);
    }
    #[test]
    fn task085_large_equivalent_sources_are_consumed_without_summed_input_refusal() {
        let sources = (0..64)
            .map(|index| source(&format!("entry-{index:02}"), "completed"))
            .collect::<Vec<_>>();
        let mut value = findings();
        value["findings"][0]["claim"] = json!("x".repeat(8192));
        value["findings"] = json!(vec![value["findings"][0].clone(); 64]);
        let bytes = encode(&value).unwrap();
        assert!(bytes.len() * 64 > MAX_BYTES as usize);
        let mut accumulator = Accumulator::new(&sources).unwrap();
        for source in &sources {
            accumulator.add(source, &bytes).unwrap();
            assert_eq!(accumulator.groups.len(), 1);
            assert_eq!(accumulator.groups.values().next().unwrap().len(), 1);
        }
        let output = accumulator.finish(&sources).unwrap();
        assert!(output.len() < 64 * 1024);
        let output: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            output["groups"][0]["variants"][0]["source_entry_keys"]
                .as_array()
                .unwrap()
                .len(),
            64
        );
    }
    #[test]
    fn task085_unique_output_capacity_is_checked_before_full_output_materialization() {
        let sources = (0..64)
            .map(|index| source(&format!("entry-{index:02}"), "completed"))
            .collect::<Vec<_>>();
        let mut accumulator = Accumulator::new(&sources).unwrap();
        let mut refused = false;
        let mut consumed = 0;
        for (index, source) in sources.iter().enumerate() {
            let mut value = findings();
            let template = value["findings"][0].clone();
            value["findings"] = json!(
                (0..64)
                    .map(|finding| {
                        let mut value = template.clone();
                        value["finding_key"] = json!(format!("finding-{index:02}-{finding:02}"));
                        value["claim"] = json!("x".repeat(8192));
                        value
                    })
                    .collect::<Vec<_>>()
            );
            consumed += 1;
            if let Err(error) = accumulator.add(source, &encode(&value).unwrap()) {
                assert_eq!(error.code, "ARTIFACT_CAPACITY_EXCEEDED");
                refused = true;
                break;
            }
        }
        assert!(refused);
        assert!(consumed < 64);
        assert!(accumulator.encoded_bytes <= MAX_BYTES as usize);
        assert_eq!(
            sources.len(),
            64,
            "complete provenance is independent of accumulator capacity"
        );
    }
}
