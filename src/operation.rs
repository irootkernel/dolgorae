//! Retained Submit identity. Reads require a verified complete ledger prefix.

use crate::audit::{AuditKind, AuditRecord};
use crate::domain::ControllerIdentity;
use crate::turn::{OperationIntent, TurnError, TurnRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Namespace {
    pub controller_principal: String,
    pub controller_generation: u64,
    pub workspace_ref: String,
    pub scope_kind: String,
    pub scope_ref: String,
    pub operation_kind: String,
    pub idempotency_key: String,
}

impl Namespace {
    pub fn submit(
        controller: &ControllerIdentity,
        workspace: &str,
        run: Uuid,
        key: &str,
    ) -> Result<Self, TurnError> {
        if key.is_empty()
            || key.len() > 256
            || controller.generation == 0
            || controller.generation > 9_007_199_254_740_991
            || !valid_opaque_ref(workspace)
        {
            return Err(TurnError::InvalidInput("operation namespace is invalid"));
        }
        let public = json!({"controller_id":controller.controller_id,"kind":controller.kind,
            "instance_id":controller.instance_id,"subject_id":controller.subject_id});
        Ok(Self {
            controller_principal: format!(
                "sha256:{}",
                crate::jcs::sha256_hex(&canonical(&public)?)
            ),
            controller_generation: controller.generation,
            workspace_ref: workspace.to_owned(),
            scope_kind: "run".to_owned(),
            scope_ref: run.to_string(),
            operation_kind: "submit_turn".to_owned(),
            idempotency_key: key.to_owned(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitIdentity {
    pub schema_version: u32,
    pub operation_ref: Uuid,
    pub mutation_envelope: Envelope,
    pub mutation_envelope_digest: String,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_revision: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub trusted_invocation: Option<Value>,
    pub state: String,
    pub accepted_subjects: Subjects,
    #[serde(deserialize_with = "required_nullable")]
    pub receipt: Option<Value>,
    #[serde(deserialize_with = "required_nullable")]
    pub definitive_refusal: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BytesRef {
    pub byte_length: u64,
    pub sha256: String,
}

impl BytesRef {
    fn of(bytes: &[u8]) -> Self {
        Self {
            byte_length: bytes.len() as u64,
            sha256: format!("sha256:{}", crate::jcs::sha256_hex(bytes)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema: String,
    pub request_schema: String,
    #[serde(flatten)]
    pub namespace: Namespace,
    #[serde(deserialize_with = "required_nullable")]
    pub trusted_invocation_ref: Option<Uuid>,
    pub request_body: BytesRef,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subjects {
    #[serde(deserialize_with = "required_nullable")]
    pub run_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub member_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub task_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub group_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub result_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub merge_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub engagement_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub maintenance_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub provisioning_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub turn_ref: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub interaction_ref: Option<String>,
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

pub(crate) fn valid_opaque_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
}

impl SubmitIdentity {
    pub(crate) fn prepare(
        operation_ref: Uuid,
        namespace: Namespace,
        request: &TurnRequest,
        effort: &str,
    ) -> Result<Self, TurnError> {
        let images = request.images.iter().map(|image| {
            let path = image.canonical_path.to_path_buf()
                .map_err(|_| TurnError::InvalidInput("image path encoding is invalid"))?;
            let canonical_path = path.to_str()
                .filter(|path| !path.is_empty() && path.len() <= 4096)
                .ok_or(TurnError::InvalidInput("successor image path must be bounded UTF-8"))?;
            Ok(json!({"detail":image.detail,"canonical_path":canonical_path,
                "content":{"byte_length":image.byte_length,"sha256":format!("sha256:{}",image.sha256)},
                "media_type":crate::turn::image_media_type(&image.canonical_path)?}))
        }).collect::<Result<Vec<_>, TurnError>>()?;
        let body = json!({"run_ref":namespace.scope_ref,
            "write_intent":if request.write {"canonical_workspace_write"} else {"read_only"},
            "message":BytesRef::of(request.message.as_bytes()),"images":images,
            "model":request.model,"effort":effort});
        let envelope = Envelope {
            schema: "dolgorae.mutation-envelope/v1".to_owned(),
            request_schema:
                "https://dolgorae.local/schema/machine-output/v5#/$defs/submit_turn_normalized_body"
                    .to_owned(),
            namespace,
            trusted_invocation_ref: None,
            request_body: BytesRef::of(&canonical(&body)?),
        };
        let digest = envelope_digest(&envelope)?;
        Ok(Self {
            schema_version: 2,
            operation_ref,
            mutation_envelope: envelope,
            mutation_envelope_digest: digest,
            expected_revision: None,
            trusted_invocation: None,
            state: "prepared".to_owned(),
            accepted_subjects: Subjects::default(),
            receipt: None,
            definitive_refusal: None,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), TurnError> {
        let envelope = &self.mutation_envelope;
        let namespace = &envelope.namespace;
        if self.schema_version != 2
            || self.operation_ref.get_version_num() != 7
            || self.state != "prepared"
            || self.receipt.is_some()
            || self.definitive_refusal.is_some()
            || self
                .expected_revision
                .is_some_and(|revision| revision > 9_007_199_254_740_991)
            || self.trusted_invocation.is_some()
            || self.accepted_subjects != Subjects::default()
            || envelope.schema != "dolgorae.mutation-envelope/v1"
            || envelope.request_schema
                != "https://dolgorae.local/schema/machine-output/v5#/$defs/submit_turn_normalized_body"
            || envelope.trusted_invocation_ref.is_some()
            || namespace.scope_kind != "run"
            || namespace.operation_kind != "submit_turn"
            || namespace.controller_generation == 0
            || namespace.controller_generation > 9_007_199_254_740_991
            || namespace.idempotency_key.is_empty()
            || namespace.idempotency_key.len() > 256
            || [
                namespace.controller_principal.as_str(),
                namespace.workspace_ref.as_str(),
                namespace.scope_ref.as_str(),
            ]
            .iter()
            .any(|value| !valid_opaque_ref(value))
            || !valid_digest(&envelope.request_body.sha256)
            || envelope.request_body.byte_length > 9_007_199_254_740_991
            || self.mutation_envelope_digest != envelope_digest(envelope)?
        {
            return Err(invalid("invalid retained Submit identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitRefusal {
    pub schema: String,
    pub operation_id: Uuid,
    pub idempotency_key: String,
    pub request_sha256: String,
    pub refusal_code: String,
}

impl SubmitRefusal {
    pub(crate) fn new(operation_id: Uuid, key: String, digest: String) -> Self {
        Self {
            schema: "dolgorae.submit-refusal/v1".to_owned(),
            operation_id,
            idempotency_key: key,
            request_sha256: digest,
            refusal_code: "APP_SERVER_REJECTED".to_owned(),
        }
    }

    pub(crate) fn validate_intent(&self, intent: &OperationIntent) -> Result<(), TurnError> {
        if self.schema != "dolgorae.submit-refusal/v1"
            || self.operation_id != intent.operation_id
            || self.idempotency_key != intent.idempotency_key
            || self.request_sha256 != intent.request_sha256
            || self.refusal_code != "APP_SERVER_REJECTED"
        {
            return Err(invalid("Submit refusal does not match its original intent"));
        }
        Ok(())
    }
}

pub(crate) fn canonical(value: &Value) -> Result<Vec<u8>, TurnError> {
    let raw = serde_json::to_string(value).map_err(|_| invalid("invalid operation JSON"))?;
    crate::jcs::canonicalize(
        &crate::jcs::parse(&raw).map_err(|_| invalid("invalid operation JSON"))?,
    )
    .map_err(|_| invalid("invalid operation JSON"))
}

fn envelope_digest(envelope: &Envelope) -> Result<String, TurnError> {
    let mut bytes = b"dolgorae.mutation-envelope/v1\n".to_vec();
    bytes.extend(canonical(
        &serde_json::to_value(envelope).map_err(|_| invalid("invalid envelope"))?,
    )?);
    Ok(format!("sha256:{}", crate::jcs::sha256_hex(&bytes)))
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn invalid(message: &str) -> TurnError {
    TurnError::Journal(message.to_owned())
}

/// The caller supplies an authenticated namespace and a complete verified
/// ledger. No filesystem attachment or external runtime is consulted.
pub(crate) fn lookup(records: &[AuditRecord], namespace: &Namespace) -> Result<Value, TurnError> {
    if records.is_empty() {
        return Err(invalid("complete operation history is unavailable"));
    }
    crate::turn::validate_submit_history(
        records
            .iter()
            .map(|record| {
                let payload = serde_json::from_slice(
                    &crate::jcs::canonicalize(record.payload())
                        .map_err(|_| invalid("invalid operation ledger payload"))?,
                )
                .map_err(|_| invalid("invalid operation ledger payload"))?;
                Ok((record.kind(), payload))
            })
            .collect::<Result<Vec<_>, TurnError>>()?,
    )?;
    let mut identities = BTreeMap::<Uuid, SubmitIdentity>::new();
    let mut intents = BTreeMap::<Uuid, OperationIntent>::new();
    let mut bound_threads = BTreeSet::new();
    let mut selected = None;
    let mut refusal = None;
    let mut acceptance = None;
    let mut outcome = "running";
    for record in records {
        let payload: Value = serde_json::from_slice(
            &crate::jcs::canonicalize(record.payload())
                .map_err(|_| invalid("invalid operation ledger payload"))?,
        )
        .map_err(|_| invalid("invalid operation ledger payload"))?;
        match record.kind() {
            AuditKind::ThreadBound => {
                if let Some(thread) = payload.get("thread_id").and_then(Value::as_str) {
                    bound_threads.insert(thread.to_owned());
                }
            }
            AuditKind::IdempotencyReserved if payload.get("operation_id").is_some() => {
                let intent: OperationIntent = serde_json::from_value(payload)
                    .map_err(|_| invalid("invalid operation reservation"))?;
                intents.insert(intent.operation_id, intent);
            }
            AuditKind::TurnIntent if payload.get("schema_version").is_some() => {
                let identity: SubmitIdentity = serde_json::from_value(payload)
                    .map_err(|_| invalid("invalid retained Submit identity"))?;
                identity.validate()?;
                if identity.mutation_envelope.namespace.scope_ref != record.run_id().to_string() {
                    return Err(invalid("Submit identity crossed its Run"));
                }
                let intent = intents
                    .get(&identity.operation_ref)
                    .ok_or_else(|| invalid("Submit identity has no original reservation"))?;
                if intent.idempotency_key != identity.mutation_envelope.namespace.idempotency_key {
                    return Err(invalid("Submit namespace changed the original key"));
                }
                if &identity.mutation_envelope.namespace == namespace {
                    if selected.is_some() && refusal.is_none() {
                        return Err(invalid("unresolved or accepted operation was redispatched"));
                    }
                    if let Some(previous) = selected.and_then(|id| identities.get(&id))
                        && previous.mutation_envelope_digest != identity.mutation_envelope_digest
                    {
                        return Err(invalid("refused key changed its original inputs"));
                    }
                    selected = Some(identity.operation_ref);
                    refusal = None;
                }
                if identities
                    .insert(identity.operation_ref, identity)
                    .is_some()
                {
                    return Err(invalid("duplicate retained Submit identity"));
                }
            }
            AuditKind::TurnIntent
                if payload.get("schema").and_then(Value::as_str)
                    == Some("dolgorae.submit-refusal/v1") =>
            {
                let resolved: SubmitRefusal = serde_json::from_value(payload)
                    .map_err(|_| invalid("invalid durable Submit refusal"))?;
                resolved.validate_intent(
                    intents
                        .get(&resolved.operation_id)
                        .ok_or_else(|| invalid("Submit refusal has no intent"))?,
                )?;
                if selected == Some(resolved.operation_id) {
                    if acceptance.is_some() {
                        return Err(invalid("accepted Submit was refused"));
                    }
                    refusal = Some((resolved, record.sequence()));
                }
            }
            AuditKind::TurnStarted
                if payload
                    .get("operation_id")
                    .and_then(Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok())
                    == selected
                    && selected.is_some() =>
            {
                if refusal.is_some() || acceptance.is_some() {
                    return Err(invalid("duplicate or refused acceptance"));
                }
                let turn = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .filter(|turn| valid_opaque_ref(turn))
                    .ok_or_else(|| invalid("accepted Submit has no Turn"))?;
                if !payload
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .is_some_and(|thread| bound_threads.contains(thread))
                    || payload
                        .get("effort")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                {
                    return Err(invalid(
                        "accepted Submit has no complete binding and admission",
                    ));
                }
                let controller: ControllerIdentity =
                    serde_json::from_value(payload["acceptance_controller"].clone())
                        .map_err(|_| invalid("accepted Submit has no Controller"))?;
                if Namespace::submit(
                    &controller,
                    &namespace.workspace_ref,
                    record.run_id(),
                    &namespace.idempotency_key,
                )
                .map_err(|_| invalid("accepted Controller identity is invalid"))?
                    != *namespace
                {
                    return Err(invalid(
                        "accepted Controller differs from original namespace",
                    ));
                }
                acceptance = Some((
                    turn.to_owned(),
                    record.sequence(),
                    record.hash().to_owned(),
                    payload
                        .get("thread_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("accepted Submit has no Thread"))?
                        .to_owned(),
                ));
            }
            AuditKind::TurnTerminal
                if acceptance.as_ref().is_some_and(|(turn, _, _, _)| {
                    payload.get("turn_id").and_then(Value::as_str) == Some(turn.as_str())
                }) =>
            {
                outcome = match payload.get("status").and_then(Value::as_str) {
                    Some("completed") => "completed",
                    Some("failed") => "failed",
                    Some("interrupted" | "cancelled") => "cancelled",
                    _ => "unknown",
                };
            }
            AuditKind::Reconciliation
                if acceptance.as_ref().is_some_and(|(turn, _, _, _)| {
                    payload.get("turn_id").and_then(Value::as_str) == Some(turn.as_str())
                }) =>
            {
                let (_, _, _, thread) = acceptance.as_ref().expect("matched acceptance");
                if payload.get("thread_id").and_then(Value::as_str) != Some(thread.as_str()) {
                    return Err(invalid("reconciliation crossed the accepted Thread"));
                }
                let observed = match payload.get("observed_status").and_then(Value::as_str) {
                    Some("completed") => Some("completed"),
                    Some("failed") => Some("failed"),
                    Some("interrupted" | "cancelled") => Some("cancelled"),
                    _ => None,
                };
                if let Some(observed) = observed {
                    if matches!(outcome, "completed" | "failed" | "cancelled")
                        && outcome != observed
                    {
                        return Err(invalid(
                            "reconciliation contradicts retained terminal proof",
                        ));
                    }
                    outcome = observed;
                } else if !matches!(outcome, "completed" | "failed" | "cancelled") {
                    outcome = "unknown";
                }
            }
            AuditKind::OutcomeUnknown if acceptance.is_some() && outcome == "running" => {
                outcome = "unknown";
            }
            _ => {}
        }
    }
    let Some(operation) = selected else {
        // A pre-successor key cannot be proven absent in the successor namespace.
        if intents
            .values()
            .any(|intent| intent.idempotency_key == namespace.idempotency_key)
        {
            return Err(TurnError::InvalidInput(
                "operation key exists outside the requested successor namespace",
            ));
        }
        return Ok(
            json!({"schema":"dolgorae.operation-lookup/v1","namespace":namespace,"state":"absent",
            "proof":{"authoritative":true,"complete":true,"retained_namespace_generation":namespace.controller_generation,
                "durable_watermark":records.last().map_or(0,AuditRecord::sequence),"tombstones_checked":true,
                "matching_operation":false,"matching_tombstone":false}}),
        );
    };
    let identity = &identities[&operation];
    if let Some((turn, revision, hash, _)) = acceptance {
        let subjects = Subjects {
            run_ref: Some(namespace.scope_ref.clone()),
            turn_ref: Some(turn),
            ..Subjects::default()
        };
        let receipt = json!({"schema":"dolgorae.operation-receipt/v1","operation_ref":operation,
            "namespace":namespace,"accepted_input_digest":identity.mutation_envelope_digest,"accepted_subjects":subjects,
            "receipt_digest":format!("sha256:{hash}"),"accepted_revision":revision,"result_digest":null});
        return Ok(
            json!({"schema":"dolgorae.operation-lookup/v1","namespace":namespace,"state":"accepted",
            "operation_ref":operation,"accepted_input_digest":identity.mutation_envelope_digest,"receipt":receipt,
            "accepted_subjects":subjects,"current_outcome":outcome,"result_ref":null,"result_digest":null}),
        );
    }
    if let Some((resolved, revision)) = refusal {
        return Ok(
            json!({"schema":"dolgorae.operation-lookup/v1","namespace":namespace,"state":"not_accepted",
            "operation_ref":operation,"input_digest":identity.mutation_envelope_digest,"refusal_code":resolved.refusal_code,
            "durable_refusal_revision":revision,"exact_key_retry_permitted":true}),
        );
    }
    Ok(
        json!({"schema":"dolgorae.operation-lookup/v1","namespace":namespace,"state":"unresolved",
        "operation_ref":operation,"known_subjects":Subjects{run_ref:Some(namespace.scope_ref.clone()),..Subjects::default()},
        "effect":"unresolved","required_action":"reconcile_operation"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ControllerKind;
    use crate::turn::{DeliveryMode, ImageDetail, ImageSnapshot};
    use std::fs;

    fn controller() -> ControllerIdentity {
        ControllerIdentity {
            controller_id: Uuid::now_v7(),
            kind: ControllerKind::HumanCli,
            instance_id: "native-test".to_owned(),
            subject_id: None,
            generation: 1,
        }
    }

    fn request() -> TurnRequest {
        TurnRequest {
            idempotency_key: "original".to_owned(),
            message: "hello".to_owned(),
            write: false,
            images: vec![],
            model: "gpt-5".to_owned(),
            effort: None,
            delivery: DeliveryMode::Submit,
        }
    }

    fn record(records: &mut Vec<AuditRecord>, run: Uuid, kind: AuditKind, payload: Value) {
        let previous = records
            .last()
            .map_or_else(|| "0".repeat(64), |record| record.hash().to_owned());
        records.push(
            AuditRecord::new(
                records.len() as u64 + 1,
                "2026-10-13T00:00:00.000000Z",
                run,
                1,
                kind,
                crate::jcs::parse(&payload.to_string()).unwrap(),
                previous,
            )
            .unwrap(),
        );
    }

    fn reserve(
        records: &mut Vec<AuditRecord>,
        run: Uuid,
        identity: &SubmitIdentity,
        request: &TurnRequest,
    ) -> OperationIntent {
        let intent = OperationIntent {
            operation_id: identity.operation_ref,
            idempotency_key: request.idempotency_key.clone(),
            request_sha256: crate::turn::normalized_request_digest(request, "medium").unwrap(),
            run_generation: 1,
            server_key: "a".repeat(64),
            server_epoch: 1,
            provisional_thread: false,
            accepted_input: None,
        };
        record(
            records,
            run,
            AuditKind::IdempotencyReserved,
            serde_json::to_value(&intent).unwrap(),
        );
        record(
            records,
            run,
            AuditKind::TurnIntent,
            serde_json::to_value(identity).unwrap(),
        );
        intent
    }

    #[test]
    fn retained_envelope_reproduces_the_frozen_h1_digest_vector() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        let vector = vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|vector| vector["id"] == "mutation-submit-turn")
            .unwrap();
        let envelope: Envelope = serde_json::from_value(vector["input"].clone()).unwrap();
        assert_eq!(
            envelope_digest(&envelope).unwrap(),
            vector["expected_digest"]
        );
        let mut invalid = vector["input"].clone();
        invalid["secret"] = json!("forbidden");
        assert!(serde_json::from_value::<Envelope>(invalid).is_err());
    }

    #[test]
    fn corrupt_accepted_controller_is_integrity_failure_instead_of_namespace_refusal() {
        for generation in [0, 9_007_199_254_740_992] {
            let run = Uuid::now_v7();
            let mut accepted_controller = controller();
            let request = request();
            let namespace = Namespace::submit(
                &accepted_controller,
                "workspace",
                run,
                &request.idempotency_key,
            )
            .unwrap();
            let identity =
                SubmitIdentity::prepare(Uuid::now_v7(), namespace.clone(), &request, "medium")
                    .unwrap();
            let mut records = Vec::new();
            reserve(&mut records, run, &identity, &request);
            record(
                &mut records,
                run,
                AuditKind::ThreadBound,
                json!({"thread_id":"original-thread"}),
            );
            accepted_controller.generation = generation;
            record(
                &mut records,
                run,
                AuditKind::TurnStarted,
                json!({
                "operation_id":identity.operation_ref,"thread_id":"original-thread",
                "turn_id":"original-turn","effort":"medium","acceptance_controller":accepted_controller}),
            );
            assert!(matches!(
                lookup(&records, &namespace),
                Err(TurnError::Journal(_))
            ));
        }
    }

    #[test]
    fn lookup_retains_original_inputs_and_receipt_after_attachment_loss() {
        let root = std::env::temp_dir().join(format!("dolgorae-operation-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("accepted.png");
        fs::write(&path, b"image bytes").unwrap();
        let mut request = request();
        request
            .images
            .push(ImageSnapshot::capture(&path, ImageDetail::High).unwrap());
        let run = Uuid::now_v7();
        let controller = controller();
        let namespace =
            Namespace::submit(&controller, "workspace", run, &request.idempotency_key).unwrap();
        let identity =
            SubmitIdentity::prepare(Uuid::now_v7(), namespace.clone(), &request, "medium").unwrap();
        let mut records = vec![];
        reserve(&mut records, run, &identity, &request);
        assert_eq!(lookup(&records, &namespace).unwrap()["state"], "unresolved");
        record(
            &mut records,
            run,
            AuditKind::ThreadBound,
            json!({"thread_id":"original-thread"}),
        );
        record(
            &mut records,
            run,
            AuditKind::TurnStarted,
            json!({"operation_id":identity.operation_ref,
            "thread_id":"original-thread","turn_id":"original-turn","effort":"medium","acceptance_controller":controller}),
        );
        let original = lookup(&records, &namespace).unwrap();
        assert_eq!(original["state"], "accepted");
        assert_eq!(original["receipt"]["accepted_revision"], 4);
        fs::rename(&path, root.join("moved.png")).unwrap();
        assert_eq!(lookup(&records, &namespace).unwrap(), original);
        fs::remove_file(root.join("moved.png")).unwrap();
        assert_eq!(lookup(&records, &namespace).unwrap(), original);
        record(
            &mut records,
            run,
            AuditKind::OutcomeUnknown,
            json!({"operation_id":Uuid::now_v7()}),
        );
        assert_eq!(
            lookup(&records, &namespace).unwrap()["current_outcome"],
            "unknown"
        );
        record(
            &mut records,
            run,
            AuditKind::TurnTerminal,
            json!({"turn_id":"original-turn",
            "status":"completed","thread_id":"original-thread","effort":"medium","usage":null,"final_response":null}),
        );
        let completed = lookup(&records, &namespace).unwrap();
        assert_eq!(completed["receipt"], original["receipt"]);
        assert_eq!(completed["current_outcome"], "completed");
        record(
            &mut records,
            run,
            AuditKind::Reconciliation,
            json!({"turn_id":"original-turn",
            "thread_id":"original-thread","observed_status":null}),
        );
        assert_eq!(
            lookup(&records, &namespace).unwrap()["current_outcome"],
            "completed"
        );
        record(
            &mut records,
            run,
            AuditKind::OutcomeUnknown,
            json!({"operation_id":Uuid::now_v7()}),
        );
        assert_eq!(
            lookup(&records, &namespace).unwrap()["current_outcome"],
            "completed"
        );
        let mut foreign = namespace.clone();
        foreign.controller_generation += 1;
        assert!(lookup(&records, &foreign).is_err());
        foreign = namespace.clone();
        foreign.controller_principal = "foreign".to_owned();
        assert!(lookup(&records, &foreign).is_err());
        let mut absent = namespace.clone();
        absent.idempotency_key = "never-submitted".to_owned();
        assert_eq!(lookup(&records, &absent).unwrap()["state"], "absent");
        assert!(lookup(&[], &absent).is_err());
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn refusal_then_retry_changes_lookup_only_after_matching_durable_acceptance() {
        let run = Uuid::now_v7();
        let controller = controller();
        let request = request();
        let namespace =
            Namespace::submit(&controller, "workspace", run, &request.idempotency_key).unwrap();
        let identity =
            SubmitIdentity::prepare(Uuid::now_v7(), namespace.clone(), &request, "medium").unwrap();
        let mut records = vec![];
        let intent = reserve(&mut records, run, &identity, &request);
        let refusal = SubmitRefusal::new(
            intent.operation_id,
            intent.idempotency_key,
            intent.request_sha256,
        );
        record(
            &mut records,
            run,
            AuditKind::TurnIntent,
            serde_json::to_value(refusal).unwrap(),
        );
        let refused = lookup(&records, &namespace).unwrap();
        assert_eq!(refused["state"], "not_accepted");
        assert_eq!(refused["durable_refusal_revision"], 3);
        let retry =
            SubmitIdentity::prepare(Uuid::now_v7(), namespace.clone(), &request, "medium").unwrap();
        reserve(&mut records, run, &retry, &request);
        assert_eq!(lookup(&records, &namespace).unwrap()["state"], "unresolved");
        record(
            &mut records,
            run,
            AuditKind::ThreadBound,
            json!({"thread_id":"thread"}),
        );
        record(
            &mut records,
            run,
            AuditKind::TurnStarted,
            json!({"operation_id":retry.operation_ref,
            "thread_id":"thread","turn_id":"turn","effort":"medium","acceptance_controller":controller}),
        );
        assert_eq!(
            lookup(&records, &namespace).unwrap()["operation_ref"],
            retry.operation_ref.to_string()
        );
        assert_eq!(lookup(&records, &namespace).unwrap()["state"], "accepted");
    }

    #[test]
    fn successor_identity_fences_namespace_and_input_drift() {
        let run = Uuid::now_v7();
        let controller = controller();
        let request = request();
        let namespace =
            Namespace::submit(&controller, "workspace", run, &request.idempotency_key).unwrap();
        let identity =
            SubmitIdentity::prepare(Uuid::now_v7(), namespace.clone(), &request, "medium").unwrap();
        let mut changed_request = request.clone();
        changed_request.write = true;
        assert_ne!(
            SubmitIdentity::prepare(
                identity.operation_ref,
                namespace.clone(),
                &changed_request,
                "medium"
            )
            .unwrap()
            .mutation_envelope_digest,
            identity.mutation_envelope_digest
        );
        let mut kind = controller.clone();
        kind.kind = ControllerKind::Other;
        assert_ne!(
            Namespace::submit(&kind, "workspace", run, &request.idempotency_key).unwrap(),
            namespace
        );
        let mut records = vec![];
        reserve(&mut records, run, &identity, &request);
        let mut corrupted = identity.clone();
        corrupted.mutation_envelope.namespace.scope_ref = Uuid::now_v7().to_string();
        let last = records.pop().unwrap();
        record(
            &mut records,
            run,
            AuditKind::TurnIntent,
            serde_json::to_value(corrupted).unwrap(),
        );
        assert!(lookup(&records, &namespace).is_err());
        records.pop();
        records.push(last);
        let mut legacy = records.clone();
        legacy.pop();
        assert!(lookup(&legacy, &namespace).is_err());
    }

    #[test]
    fn retained_identity_requires_closed_fields_and_explicit_nulls() {
        let run = Uuid::now_v7();
        let request = request();
        let namespace =
            Namespace::submit(&controller(), "workspace", run, &request.idempotency_key).unwrap();
        let identity =
            SubmitIdentity::prepare(Uuid::now_v7(), namespace, &request, "medium").unwrap();
        let value = serde_json::to_value(&identity).unwrap();
        for field in [
            "expected_revision",
            "trusted_invocation",
            "receipt",
            "definitive_refusal",
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<SubmitIdentity>(missing).is_err(),
                "{field}"
            );
        }
        for field in [
            "run_ref",
            "member_ref",
            "task_ref",
            "group_ref",
            "result_ref",
            "merge_ref",
            "engagement_ref",
            "maintenance_ref",
            "provisioning_ref",
            "turn_ref",
            "interaction_ref",
        ] {
            let mut missing = value.clone();
            missing["accepted_subjects"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                serde_json::from_value::<SubmitIdentity>(missing).is_err(),
                "{field}"
            );
        }
        for pointer in [
            "/foreign",
            "/mutation_envelope/foreign",
            "/accepted_subjects/foreign",
            "/mutation_envelope/request_body/foreign",
        ] {
            let mut invalid = value.clone();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            invalid
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), json!("forbidden"));
            assert!(
                serde_json::from_value::<SubmitIdentity>(invalid).is_err(),
                "{pointer}"
            );
        }
        for generation in [0, 9_007_199_254_740_992] {
            let mut invalid = identity.clone();
            invalid.mutation_envelope.namespace.controller_generation = generation;
            invalid.mutation_envelope_digest = envelope_digest(&invalid.mutation_envelope).unwrap();
            assert!(invalid.validate().is_err());
        }
        let mut invalid = identity;
        invalid.schema_version = 3;
        assert!(invalid.validate().is_err());
    }
}
