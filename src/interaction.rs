//! Transport-neutral durable Interaction observations and protected response bounds.
use crate::audit::AuditKind;
use crate::domain::RunLifecycle;
use crate::ledger::{ObservedLedger, audit_integrity_error};
use crate::machine::MachineError;
use crate::snapshot::RunSnapshot;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub(crate) const MAX_RESPONSE_BYTES: usize = 1_048_576;
pub(crate) const MAX_PAYLOAD_BYTES: usize = 8_388_608;

pub(crate) fn durable_interactions_at(
    state_root: &Path,
    run_id: Uuid,
    head: u64,
) -> Result<Vec<(Value, u64)>, MachineError> {
    let ledger = ObservedLedger::open_run(state_root, run_id, head)?;
    durable_interactions_from_ledger(&ledger, run_id, head)
}

pub(crate) fn durable_interactions_from_ledger(
    ledger: &ObservedLedger,
    run_id: Uuid,
    head: u64,
) -> Result<Vec<(Value, u64)>, MachineError> {
    let mut interactions: Vec<(Value, u64)> = Vec::new();
    let mut latest = std::collections::BTreeMap::new();
    for record in ledger.records() {
        if !matches!(
            record.kind(),
            AuditKind::ApprovalRequested | AuditKind::ApprovalDecided
        ) {
            continue;
        }
        let bytes = crate::jcs::canonicalize(record.payload())
            .map_err(|error| audit_integrity_error(run_id, head, &error.to_string()))?;
        let mut payload: Value = serde_json::from_slice(&bytes)
            .map_err(|error| audit_integrity_error(run_id, head, &error.to_string()))?;
        if record.kind() == AuditKind::ApprovalRequested {
            let upstream_id = payload
                .get("upstream_request_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("durable interaction upstream id"))?;
            let interaction = payload
                .get_mut("interaction")
                .map(Value::take)
                .ok_or_else(|| invalid("durable normalized interaction"))?;
            latest.insert(upstream_id, interactions.len());
            interactions.push((interaction, upstream_id));
        } else if let Some(index) = payload
            .get("request_id")
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<u64>().ok())
            .and_then(|id| latest.get(&id))
        {
            let object = interactions[*index]
                .0
                .as_object_mut()
                .ok_or_else(|| invalid("durable normalized interaction"))?;
            let resolution = payload
                .get_mut("resolution")
                .map(Value::take)
                .ok_or_else(|| invalid("durable interaction resolution"))?;
            object.insert(
                "status".to_owned(),
                json!(
                    if resolution.get("outcome").and_then(Value::as_str) == Some("stale") {
                        "stale"
                    } else {
                        "resolved"
                    }
                ),
            );
            object.insert(
                "resolved_at".to_owned(),
                payload
                    .get_mut("resolved_at")
                    .map(Value::take)
                    .unwrap_or(Value::Null),
            );
            object.insert("resolution".to_owned(), resolution);
        }
    }
    Ok(interactions)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProofKind {
    Approval,
    UserInput,
    Unsupported,
}

impl ProofKind {
    fn of(kind: &str) -> Result<Self, MachineError> {
        match kind {
            "command_execution_approval" | "file_change_approval" => Ok(Self::Approval),
            "user_input" => Ok(Self::UserInput),
            "unsupported_request" => Ok(Self::Unsupported),
            _ => Err(invalid("closed proof interaction kind")),
        }
    }
}

#[derive(Clone, Copy)]
enum ResolutionSource {
    Normalized,
    Journal,
}

fn proof_uuid(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        Uuid::try_parse(text).is_ok_and(|id| {
            id.get_variant() == uuid::Variant::RFC4122
                && id.get_version_num() == 7
                && id.to_string() == text
        })
    })
}

fn proof_digest(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn proof_date(value: &str) -> bool {
    // The existing protobuf parser validates calendar/time values but also accepts
    // date-only, space-separated and abbreviated-offset inputs. Exclude those
    // extensions before using it for the interaction's RFC3339 date-time contract.
    let bytes = value.as_bytes();
    if !value.is_ascii()
        || bytes.len() < 20
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || !matches!(bytes[10], b'T' | b't')
    {
        return false;
    }
    let end = if matches!(bytes.last(), Some(b'Z' | b'z')) {
        bytes.len() - 1
    } else if bytes.len() >= 25
        && matches!(bytes[bytes.len() - 6], b'+' | b'-')
        && bytes[bytes.len() - 3] == b':'
        && bytes[bytes.len() - 5..bytes.len() - 3]
            .iter()
            .chain(&bytes[bytes.len() - 2..])
            .all(u8::is_ascii_digit)
    {
        bytes.len() - 6
    } else {
        return false;
    };
    if !(end == 19
        || (end > 20 && bytes[19] == b'.' && bytes[20..end].iter().all(u8::is_ascii_digit)))
    {
        return false;
    }
    let Ok(parsed) = value.parse::<prost_types::Timestamp>() else {
        return false;
    };
    if &bytes[17..19] != b"60" {
        return true;
    }
    // prost normalizes every :60 to :59. RFC3339 5.7 requires a leap second
    // at the UTC June/December boundary, including when the input has an offset.
    // This checks the position, without predicting leap-second announcements.
    let utc = parsed.to_string();
    utc.len() >= 19 && &utc[11..19] == "23:59:59" && matches!(&utc[5..10], "06-30" | "12-31")
}

/// Check a closed resolution arm without retaining protected request/answer data.
/// Journal decisions have two source-backed differences from the normalized DTO.
fn proof_resolution(
    value: &Value,
    kind: Option<ProofKind>,
    status: Option<&str>,
    source: ResolutionSource,
) -> Result<(), MachineError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("interaction resolution object"))?;
    let keys = |expected: &[&str]| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    };
    let journal = matches!(source, ResolutionSource::Journal);
    let (arm_kind, arm_status, valid) = match object.get("outcome").and_then(Value::as_str) {
        Some("approval") => (
            Some(ProofKind::Approval),
            "resolved",
            keys(&["outcome", "decision"])
                && matches!(
                    value["decision"].as_str(),
                    Some("accept_once" | "decline" | "cancel")
                ),
        ),
        Some("answered") => {
            let secret = value["contained_secret"].as_bool();
            let receipt = &value["resolution_receipt_id"];
            (
                Some(ProofKind::UserInput),
                "resolved",
                keys(&[
                    "outcome",
                    "contained_secret",
                    "answer_digest",
                    "answer_count",
                    "resolution_receipt_id",
                ]) && value["answer_count"]
                    .as_u64()
                    .is_some_and(|n| (1..=crate::domain::MAX_JCS_SAFE_INTEGER).contains(&n))
                    && match secret {
                        Some(true) => value["answer_digest"].is_null() && proof_uuid(receipt),
                        Some(false) => {
                            proof_digest(&value["answer_digest"])
                                && (receipt.is_null() || (journal && proof_uuid(receipt)))
                        }
                        None => false,
                    },
            )
        }
        Some("stale") => (
            None,
            "stale",
            keys(&["outcome", "reason"])
                && (matches!(
                    value["reason"].as_str(),
                    Some(
                        "upstream_cleared"
                            | "run_generation_changed"
                            | "server_epoch_changed"
                            | "turn_terminal"
                            | "file_change_snapshot_changed"
                            | "file_change_artifact_unavailable"
                    )
                ) || (journal && value["reason"] == "terminal_turn"))
                && kind != Some(ProofKind::Unsupported),
        ),
        Some("method_not_found") => (
            Some(ProofKind::Unsupported),
            "resolved",
            !journal
                && keys(&["outcome", "reply_code"])
                && value["reply_code"].as_i64() == Some(-32601),
        ),
        _ => return Err(invalid("closed interaction resolution outcome")),
    };
    if !valid
        || kind
            .zip(arm_kind)
            .is_some_and(|(actual, expected)| actual != expected)
        || status.is_some_and(|actual| actual != arm_status)
    {
        return Err(invalid("interaction resolution arm, kind and status"));
    }
    Ok(())
}

fn proof_decision(value: &Value, kind: Option<ProofKind>) -> Result<(), MachineError> {
    let resolution = value
        .get("resolution")
        .ok_or_else(|| invalid("interaction decision resolution"))?;
    proof_resolution(resolution, kind, None, ResolutionSource::Journal)?;
    if !value
        .get("resolved_at")
        .and_then(Value::as_str)
        .is_some_and(proof_date)
        || value
            .get("idempotency_key")
            .and_then(Value::as_str)
            .is_none()
    {
        return Err(invalid("interaction decision date and key"));
    }
    let digest = value.get("response_sha256").unwrap_or(&Value::Null);
    let receipt = value.get("resolution_receipt_id").unwrap_or(&Value::Null);
    // restore_interaction_resolutions and approval_decisions retain early v2
    // nonsecret decisions without receipts. Absence proof never mints a receipt.
    if !(receipt.is_null() || proof_uuid(receipt)) {
        return Err(invalid("interaction decision receipt"));
    }
    match resolution["outcome"].as_str() {
        Some("stale") if digest.is_null() && receipt.is_null() => {}
        Some("approval") if proof_digest(digest) => {}
        Some("answered")
            if digest == &resolution["answer_digest"]
                && receipt == &resolution["resolution_receipt_id"] => {}
        _ => return Err(invalid("interaction decision digest and receipt linkage")),
    }
    Ok(())
}

/// Validate the original proof metadata before UUID conversion can normalize it.
/// Existing display readers keep their own decoder and versioned meaning.
fn parse_complete_interaction(raw: Value, run_id: Uuid) -> Result<Interaction, MachineError> {
    let object = raw
        .as_object()
        .ok_or_else(|| invalid("normalized interaction object"))?;
    for field in ["item_id", "resolved_at", "resolution"] {
        if !object.contains_key(field) {
            return Err(invalid("explicit normalized interaction nullable fields"));
        }
    }
    // These two fields have the checked canonical UUIDv7 contract.
    // request_id retains the existing Interaction parser's meaning.
    for field in ["run_id", "controller_id"] {
        let text = object
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("canonical interaction authority identity"))?;
        let id = Uuid::try_parse(text)
            .map_err(|_| invalid("canonical interaction authority identity"))?;
        if id.get_variant() != uuid::Variant::RFC4122
            || id.get_version_num() != 7
            || id.to_string() != text
        {
            return Err(invalid("canonical interaction authority identity"));
        }
    }
    let interaction = Interaction::parse(raw, run_id)?;
    if !proof_date(&interaction.opened_at)
        || interaction
            .resolved_at
            .as_deref()
            .is_some_and(|date| !proof_date(date))
    {
        return Err(invalid("interaction date-time"));
    }
    if let Some(resolution) = &interaction.resolution {
        proof_resolution(
            resolution,
            Some(ProofKind::of(&interaction.kind)?),
            Some(&interaction.status),
            ResolutionSource::Normalized,
        )?;
    }
    Ok(interaction)
}

/// Reduce only upstream correlation and pending status. Protected interaction
/// payloads are decoded one record at a time and never retained in this map.
pub(crate) fn complete_pending(
    ledger: &ObservedLedger,
    manifest: &crate::run::RunManifest,
    projection: &crate::projection::RunStateProjection,
    controller: &crate::run::ControllerBinding,
    app_server_epoch: Option<u64>,
    budget: &crate::ledger::ObservationBudget,
    validate_payload: impl Fn(&Interaction) -> Result<(), MachineError>,
) -> Result<(bool, u64), MachineError> {
    let mut revision = 0;
    let mut requests = std::collections::BTreeMap::<u64, (bool, ProofKind)>::new();
    let mut raw_open = std::collections::BTreeSet::new();
    for record in ledger.records() {
        budget
            .check()
            .map_err(|e| crate::ledger::observation_error(manifest.run_id, record.sequence(), e))?;
        revision = crate::ledger::next_interaction_revision(revision, record);
        if !matches!(
            record.kind(),
            AuditKind::ApprovalRequested
                | AuditKind::ApprovalDecided
                | AuditKind::InteractionOpened
                | AuditKind::InteractionResolved
        ) {
            continue;
        }
        let bytes = crate::jcs::canonicalize(record.payload())
            .map_err(|_| invalid("interaction payload encoding"))?;
        let mut value: Value =
            serde_json::from_slice(&bytes).map_err(|_| invalid("interaction payload"))?;
        match record.kind() {
            AuditKind::ApprovalDecided => {
                let id = value
                    .get("request_id")
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or_else(|| invalid("interaction decision correlation"))?;
                // Decisions close the preceding request, never a later reuse of its ID.
                proof_decision(&value, requests.get(&id).map(|(_, kind)| *kind))?;
                requests.remove(&id);
            }
            AuditKind::ApprovalRequested => {
                let id = value
                    .get("upstream_request_id")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| invalid("interaction upstream correlation"))?;
                let raw = value
                    .get_mut("interaction")
                    .map(Value::take)
                    .ok_or_else(|| invalid("normalized interaction"))?;
                let interaction = parse_complete_interaction(raw, manifest.run_id)?;
                validate_payload(&interaction)?;
                if interaction.run_generation != record.run_generation() {
                    return Err(invalid("interaction audit generation linkage"));
                }
                if app_server_epoch.is_none()
                    && interaction.pending_at_context(PendingContext {
                        run_generation: projection.run_generation,
                        controller_id: controller.identity.controller_id,
                        control_mode: manifest.control_mode,
                        app_server_epoch: Some(interaction.server_epoch),
                        active_turn_id: projection.active_turn_id.as_deref(),
                        lifecycle: projection.lifecycle,
                    })
                {
                    return Err(MachineError::new(
                        "RECOVERY_REQUIRED",
                        "pending interaction has no observable current App Server epoch",
                        false,
                        json!({"required_action":"restore_state"}),
                    ));
                }
                let pending = interaction.pending_at_context(PendingContext {
                    run_generation: projection.run_generation,
                    controller_id: controller.identity.controller_id,
                    control_mode: manifest.control_mode,
                    app_server_epoch,
                    active_turn_id: projection.active_turn_id.as_deref(),
                    lifecycle: projection.lifecycle,
                });
                requests.insert(id, (pending, ProofKind::of(&interaction.kind)?));
            }
            AuditKind::InteractionOpened | AuditKind::InteractionResolved => {
                let id = value
                    .get("request_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("raw interaction correlation"))?;
                if record.kind() == AuditKind::InteractionOpened {
                    raw_open.insert((record.run_generation(), id.to_owned()));
                } else {
                    raw_open.remove(&(record.run_generation(), id.to_owned()));
                }
            }
            _ => unreachable!(),
        }
    }
    budget
        .check()
        .map_err(|e| crate::ledger::observation_error(manifest.run_id, ledger.head(), e))?;
    Ok((
        requests.values().any(|(pending, _)| *pending)
            || raw_open
                .iter()
                .any(|(generation, _)| *generation == projection.run_generation),
        revision,
    ))
}

pub(crate) fn response_too_large(
    run_id: impl serde::Serialize,
    request_id: &str,
    observed_bytes: usize,
    message: &str,
) -> MachineError {
    MachineError::new(
        "INTERACTION_RESPONSE_TOO_LARGE",
        message,
        false,
        json!({"run_id":run_id,"request_id":request_id,"observed_bytes":observed_bytes,"limit_bytes":MAX_RESPONSE_BYTES}),
    )
}

fn invalid(invariant: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable observation violates its checked shape",
        false,
        json!({"invariant":invariant}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Interaction {
    pub(crate) schema_version: u32,
    pub(crate) request_id: Uuid,
    pub(crate) run_id: Uuid,
    pub(crate) controller_id: Uuid,
    pub(crate) control_mode: crate::domain::ControlMode,
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) item_id: Option<String>,
    pub(crate) run_generation: u64,
    pub(crate) server_epoch: u64,
    pub(crate) kind: String,
    pub(crate) status: String,
    pub(crate) payload: Value,
    pub(crate) available_decisions: Vec<String>,
    pub(crate) response_schema: String,
    pub(crate) opened_at: String,
    pub(crate) resolved_at: Option<String>,
    pub(crate) resolution: Option<Value>,
}

impl Interaction {
    pub(crate) fn parse(value: Value, run_id: Uuid) -> Result<Self, MachineError> {
        let interaction: Self =
            serde_json::from_value(value).map_err(|_| invalid("normalized interaction DTO"))?;
        if interaction.schema_version != 1
            || interaction.request_id.get_version_num() != 7
            || interaction.run_id != run_id
            || interaction.controller_id.get_version_num() != 7
            || interaction.thread_id.is_empty()
            || interaction.thread_id.len() > 256
            || interaction.turn_id.is_empty()
            || interaction.turn_id.len() > 256
            || interaction
                .item_id
                .as_ref()
                .is_some_and(|id| id.len() > 256)
            || interaction.run_generation == 0
            || interaction.run_generation > crate::domain::MAX_JCS_SAFE_INTEGER
            || interaction.server_epoch == 0
            || interaction.server_epoch > crate::domain::MAX_JCS_SAFE_INTEGER
            || !matches!(
                interaction.kind.as_str(),
                "command_execution_approval"
                    | "file_change_approval"
                    | "user_input"
                    | "unsupported_request"
            )
            || !matches!(
                interaction.status.as_str(),
                "pending" | "resolved" | "stale"
            )
            || (interaction.status == "pending"
                && (interaction.resolved_at.is_some() || interaction.resolution.is_some()))
            || (interaction.status != "pending"
                && (interaction.resolved_at.is_none() || interaction.resolution.is_none()))
        {
            return Err(invalid("normalized interaction identity and state"));
        }
        Ok(interaction)
    }

    pub(crate) fn require_bounded_payload(&self) -> Result<(), MachineError> {
        let observed_bytes = serde_json::to_vec(&self.payload)
            .map_err(|_| invalid("normalized interaction payload encoding"))?
            .len();
        if observed_bytes > MAX_PAYLOAD_BYTES {
            return Err(MachineError::new(
                "INTERACTION_PAYLOAD_TOO_LARGE",
                "the typed safe interaction payload exceeds 8 MiB",
                false,
                json!({
                    "run_id": self.run_id,
                    "request_id": self.request_id,
                    "observed_bytes": observed_bytes,
                    "limit_bytes": MAX_PAYLOAD_BYTES,
                }),
            ));
        }
        Ok(())
    }

    pub(crate) fn pending_at(&self, snapshot: &RunSnapshot) -> bool {
        self.pending_at_context(PendingContext {
            run_generation: snapshot.projection.run_generation,
            controller_id: snapshot.controller.controller_id,
            control_mode: snapshot.manifest.control_mode,
            app_server_epoch: snapshot.app_server_epoch,
            active_turn_id: snapshot.projection.active_turn_id.as_deref(),
            lifecycle: snapshot.projection.lifecycle,
        })
    }

    fn pending_at_context(&self, context: PendingContext<'_>) -> bool {
        self.status == "pending"
            && self.run_generation == context.run_generation
            && self.controller_id == context.controller_id
            && self.control_mode == context.control_mode
            && context.app_server_epoch == Some(self.server_epoch)
            && context.active_turn_id == Some(&self.turn_id)
            && !matches!(
                context.lifecycle,
                RunLifecycle::Closed | RunLifecycle::StartFailed | RunLifecycle::OutcomeUnknown
            )
    }

    pub(crate) fn observer_summary(
        &self,
        status: InteractionStatus,
    ) -> Result<ObserverSummary, MachineError> {
        let protected = self.kind == "user_input"
            && serde_json::from_value::<UserInput>(self.payload.clone())
                .map_err(|_| invalid("user input questions"))?
                .questions
                .iter()
                .any(|question| question.is_secret);
        let title = match (status, self.kind.as_str(), protected) {
            (InteractionStatus::Stale, _, _) => "Interaction stale",
            (InteractionStatus::Resolved, _, _) => "Interaction resolved",
            (_, "command_execution_approval", _) => "Command approval requested",
            (_, "file_change_approval", _) => "File change approval requested",
            (_, "user_input", true) => "Protected input requested",
            (_, "user_input", false) => "User input requested",
            _ => return Err(invalid("observer-safe interaction title")),
        };
        Ok(ObserverSummary {
            safe_title: title,
            contains_protected_input: protected,
            requires_user_escalation: status == InteractionStatus::Pending,
        })
    }
}

#[derive(Clone)]
struct PendingContext<'a> {
    run_generation: u64,
    controller_id: Uuid,
    control_mode: crate::domain::ControlMode,
    app_server_epoch: Option<u64>,
    active_turn_id: Option<&'a str>,
    lifecycle: RunLifecycle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractionStatus {
    Pending,
    Resolved,
    Stale,
}

pub(crate) struct ObserverSummary {
    pub(crate) safe_title: &'static str,
    pub(crate) contains_protected_input: bool,
    pub(crate) requires_user_escalation: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserInput {
    pub(crate) is_blocking: bool,
    pub(crate) questions: Vec<Question>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Question {
    pub(crate) id: String,
    pub(crate) header: String,
    pub(crate) question: String,
    pub(crate) is_other: bool,
    pub(crate) is_secret: bool,
    pub(crate) options: Option<Vec<QuestionOption>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QuestionOption {
    pub(crate) label: String,
    pub(crate) description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_interaction() -> Interaction {
        serde_json::from_value(json!({
            "schema_version": 1,
            "request_id": Uuid::now_v7(),
            "run_id": Uuid::now_v7(),
            "controller_id": Uuid::now_v7(),
            "control_mode": "direct_interactive",
            "thread_id": "thread",
            "turn_id": "turn",
            "item_id": null,
            "run_generation": 7,
            "server_epoch": 11,
            "kind": "command_execution_approval",
            "status": "pending",
            "payload": {},
            "available_decisions": [],
            "response_schema": "dolgorae.interaction.command-execution/v1",
            "opened_at": "2026-09-09T00:00:00.123456Z",
            "resolved_at": null,
            "resolution": null
        }))
        .unwrap()
    }

    fn matching_context(interaction: &Interaction) -> PendingContext<'static> {
        PendingContext {
            run_generation: interaction.run_generation,
            controller_id: interaction.controller_id,
            control_mode: interaction.control_mode,
            app_server_epoch: Some(interaction.server_epoch),
            active_turn_id: Some("turn"),
            lifecycle: RunLifecycle::WaitingInteraction,
        }
    }

    #[test]
    fn complete_observation_original_interaction_metadata_is_strict_before_conversion() {
        let run = Uuid::parse_str("018faaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee").unwrap();
        let controller = Uuid::parse_str("018fffff-abcd-7abc-9abc-abcdefabcdef").unwrap();
        let valid = json!({"schema_version":1,"request_id":Uuid::now_v7(),"run_id":run,"controller_id":controller,"control_mode":"direct_interactive","thread_id":"thread","turn_id":"turn","item_id":null,"run_generation":1,"server_epoch":1,"kind":"command_execution_approval","status":"resolved","payload":{"title":"Approval","message":"Inspect the command.","command":["true"],"cwd":"/fixture","reason":null},"available_decisions":["accept_once","decline","cancel"],"response_schema":"dolgorae.interaction.command-approval/v1","opened_at":"2026-10-26T00:00:00.000000Z","resolved_at":"2026-10-26T00:00:01.000000Z","resolution":{"outcome":"approval","decision":"accept_once"}});
        let parsed = parse_complete_interaction(valid.clone(), run).unwrap();
        crate::interaction_payload::validate(&parsed).unwrap();
        for field in ["run_id", "controller_id"] {
            let text = valid[field].as_str().unwrap();
            for input in [
                json!(text.replace('-', "")),
                json!(text.to_ascii_uppercase()),
                json!(format!("{{{text}}}")),
                json!(format!("urn:uuid:{text}")),
                json!("018f1111-2222-4333-8444-555555555555"),
                json!("018f1111-2222-7333-0444-555555555555"),
                Value::Null,
                json!(7),
            ] {
                let mut candidate = valid.clone();
                candidate[field] = input;
                assert!(
                    parse_complete_interaction(candidate, run).is_err(),
                    "{field} cannot normalize original proof text"
                );
            }
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(parse_complete_interaction(missing, run).is_err());
        }
        for field in ["item_id", "resolved_at", "resolution"] {
            let mut candidate = valid.clone();
            candidate.as_object_mut().unwrap().remove(field);
            assert!(parse_complete_interaction(candidate, run).is_err());
        }
        for field in ["response_schema", "payload", "available_decisions"] {
            let mut candidate = valid.clone();
            candidate[field] = match field {
                "response_schema" => json!("other"),
                "payload" => json!({}),
                _ => json!([]),
            };
            let interaction = parse_complete_interaction(candidate, run).unwrap();
            assert!(crate::interaction_payload::validate(&interaction).is_err());
        }
    }

    #[test]
    fn complete_proof_resolution_arms_are_closed_and_source_aware() {
        for journal in [false, true] {
            let source = if journal {
                ResolutionSource::Journal
            } else {
                ResolutionSource::Normalized
            };
            for (kind, status, resolution) in proof_fixtures::resolutions(journal) {
                let kind = ProofKind::of(kind).unwrap();
                proof_resolution(&resolution, Some(kind), Some(status), source).unwrap();
                if resolution["outcome"] == "answered" {
                    for count in [1, crate::domain::MAX_JCS_SAFE_INTEGER] {
                        let mut edge = resolution.clone();
                        edge["answer_count"] = json!(count);
                        proof_resolution(&edge, Some(kind), Some(status), source).unwrap();
                    }
                }
                for bad in proof_fixtures::malformed(&resolution) {
                    assert!(
                        proof_resolution(&bad, Some(kind), Some(status), source).is_err(),
                        "{journal}/{status}/{bad}"
                    );
                }
                for other in [
                    ProofKind::Approval,
                    ProofKind::UserInput,
                    ProofKind::Unsupported,
                ] {
                    if other != kind && resolution["outcome"] != "stale" {
                        assert!(
                            proof_resolution(&resolution, Some(other), Some(status), source)
                                .is_err()
                        );
                    }
                }
                for wrong in [
                    if status == "resolved" {
                        "stale"
                    } else {
                        "resolved"
                    },
                    "pending",
                ] {
                    assert!(
                        proof_resolution(&resolution, Some(kind), Some(wrong), source).is_err()
                    );
                }
            }
        }
        for resolution in [
            json!({"outcome":"stale","reason":"terminal_turn"}),
            json!({"outcome":"answered","contained_secret":false,"answer_digest":"a".repeat(64),"answer_count":1,"resolution_receipt_id":"018faaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee"}),
        ] {
            assert!(
                proof_resolution(&resolution, None, None, ResolutionSource::Normalized).is_err()
            );
            proof_resolution(&resolution, None, None, ResolutionSource::Journal).unwrap();
        }
    }

    #[test]
    fn complete_proof_leap_seconds_require_the_utc_boundary() {
        for valid in [
            "1990-12-31T23:59:60Z",
            "1990-12-31T15:59:60-08:00",
            "2017-01-01T08:59:60.123456+09:00",
            "2015-07-01T01:59:60+02:00",
            "2016-12-31t23:59:60z",
        ] {
            assert!(
                proof_date(valid),
                "legitimate leap-second position: {valid}"
            );
        }
        for invalid in [
            "2026-10-26T00:00:60Z",
            "2016-12-31T23:58:60Z",
            "2016-12-30T23:59:60Z",
            "2016-11-30T23:59:60Z",
            "2016-12-31T23:59:60+01:00",
            "2017-01-01T08:59:60+08:00",
        ] {
            assert!(
                !proof_date(invalid),
                "original :60 must be at a UTC leap boundary: {invalid}"
            );
        }
    }

    #[test]
    fn complete_proof_raw_metadata_and_historical_receipts_are_checked() {
        for (kind, _, resolution) in proof_fixtures::resolutions(true) {
            let kind = Some(ProofKind::of(kind).unwrap());
            let digest = match resolution["outcome"].as_str().unwrap() {
                "stale" => Value::Null,
                "answered" => resolution["answer_digest"].clone(),
                _ => json!(crate::jcs::sha256_hex(
                    &crate::jcs::canonicalize(
                        &crate::jcs::parse(&json!({"decision":resolution["decision"]}).to_string())
                            .unwrap()
                    )
                    .unwrap()
                )),
            };
            let receipt = match resolution["outcome"].as_str().unwrap() {
                "stale" => Value::Null,
                "answered" => resolution["resolution_receipt_id"].clone(),
                _ => json!(Uuid::now_v7()),
            };
            let valid = json!({"request_id":"1","idempotency_key":if resolution["outcome"]=="stale" {""} else {"fixture"},"resolved_at":"2026-10-26T00:00:01.000000Z","response_sha256":digest,"resolution_receipt_id":receipt,"resolution":resolution});
            proof_decision(&valid, kind).unwrap();
            if valid["resolution"]["outcome"] == "approval"
                || (valid["resolution"]["outcome"] == "answered"
                    && valid["resolution"]["resolution_receipt_id"].is_null())
            {
                let mut historical = valid.clone();
                historical
                    .as_object_mut()
                    .unwrap()
                    .remove("resolution_receipt_id");
                proof_decision(&historical, kind).unwrap();
                historical["resolution_receipt_id"] = Value::Null;
                proof_decision(&historical, kind).unwrap();
            }
            for field in ["resolved_at", "idempotency_key"] {
                let mut missing = valid.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert!(proof_decision(&missing, kind).is_err(), "missing {field}");
                for bad in [Value::Null, json!(true), json!(1), json!([]), json!({})] {
                    let mut candidate = valid.clone();
                    candidate[field] = bad;
                    assert!(
                        proof_decision(&candidate, kind).is_err(),
                        "{field}: {candidate}"
                    );
                }
            }
            for bad in proof_fixtures::invalid_dates() {
                let mut candidate = valid.clone();
                candidate["resolved_at"] = bad;
                assert!(proof_decision(&candidate, kind).is_err(), "{candidate}");
            }
            for bad in [
                json!("bad"),
                json!(true),
                json!({}),
                json!("018FAAAA-BBBB-7CCC-8DDD-EEEEEEEEEEEE"),
                json!("018faaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
            ] {
                let mut candidate = valid.clone();
                candidate["resolution_receipt_id"] = bad;
                assert!(proof_decision(&candidate, kind).is_err(), "{candidate}");
            }
            for bad in [
                json!("g".repeat(64)),
                json!("A".repeat(64)),
                json!(true),
                json!("a".repeat(63)),
                json!("b".repeat(64)),
            ] {
                if valid["resolution"]["outcome"] == "approval" && bad == json!("b".repeat(64)) {
                    continue;
                }
                let mut candidate = valid.clone();
                candidate["response_sha256"] = bad;
                assert!(proof_decision(&candidate, kind).is_err(), "{candidate}");
            }
        }
        assert!(
            proof_resolution(
                &json!({"outcome":"stale","reason":"turn_terminal"}),
                Some(ProofKind::Unsupported),
                Some("stale"),
                ResolutionSource::Normalized
            )
            .is_err()
        );
        assert!(
            proof_resolution(
                &json!({"outcome":"method_not_found","reply_code":-32601}),
                None,
                None,
                ResolutionSource::Journal
            )
            .is_err()
        );
    }

    #[test]
    fn complete_proof_dates_keep_rfc3339_and_reject_parser_extensions() {
        for valid in [
            "2024-02-29T23:59:59Z",
            "2026-10-26t09:00:00.123456789012+09:00",
            "2026-10-26T00:00:00.000000Z",
            "2026-10-25T19:00:00-05:00",
            "2026-10-26T00:00:00z",
        ] {
            assert!(proof_date(valid), "{valid}");
        }
        for bad in proof_fixtures::invalid_dates() {
            assert!(!bad.as_str().is_some_and(proof_date), "{bad}");
        }
    }

    #[test]
    fn pending_at_rejects_every_snapshot_mismatch_and_terminal_lifecycle() {
        let mut interaction = pending_interaction();
        let matching = matching_context(&interaction);
        assert!(interaction.pending_at_context(matching.clone()));

        interaction.status = "resolved".to_owned();
        assert!(!interaction.pending_at_context(matching.clone()));
        interaction.status = "pending".to_owned();

        let mut mismatched = matching.clone();
        mismatched.run_generation += 1;
        assert!(!interaction.pending_at_context(mismatched));

        let mut mismatched = matching.clone();
        mismatched.controller_id = Uuid::now_v7();
        assert!(!interaction.pending_at_context(mismatched));

        let mut mismatched = matching.clone();
        mismatched.control_mode = crate::domain::ControlMode::ManagedAgent;
        assert!(!interaction.pending_at_context(mismatched));

        let mut mismatched = matching.clone();
        mismatched.app_server_epoch = Some(interaction.server_epoch + 1);
        assert!(!interaction.pending_at_context(mismatched));

        let mut mismatched = matching.clone();
        mismatched.app_server_epoch = None;
        assert!(!interaction.pending_at_context(mismatched));

        let mut mismatched = matching.clone();
        mismatched.active_turn_id = Some("different-turn");
        assert!(!interaction.pending_at_context(mismatched));

        for lifecycle in [
            RunLifecycle::Closed,
            RunLifecycle::StartFailed,
            RunLifecycle::OutcomeUnknown,
        ] {
            let mut terminal = matching.clone();
            terminal.lifecycle = lifecycle;
            assert!(!interaction.pending_at_context(terminal));
        }
    }

    #[test]
    fn encoded_payload_above_advertised_limit_is_rejected_with_exact_measurement() {
        let mut interaction = pending_interaction();
        interaction.payload = json!({"message": "x".repeat(MAX_PAYLOAD_BYTES)});
        let expected = serde_json::to_vec(&interaction.payload).unwrap().len();
        let error = interaction.require_bounded_payload().unwrap_err();
        assert_eq!(error.code, "INTERACTION_PAYLOAD_TOO_LARGE");
        assert_eq!(error.details["run_id"], json!(interaction.run_id));
        assert_eq!(error.details["request_id"], json!(interaction.request_id));
        assert_eq!(error.details["observed_bytes"], expected);
        assert_eq!(error.details["limit_bytes"], MAX_PAYLOAD_BYTES);
    }
}

#[cfg(test)]
pub(crate) mod proof_fixtures {
    use serde_json::{Value, json};

    pub(crate) fn resolutions(journal: bool) -> Vec<(&'static str, &'static str, Value)> {
        let mut cases = Vec::new();
        for kind in ["command_execution_approval", "file_change_approval"] {
            for decision in ["accept_once", "decline", "cancel"] {
                cases.push((
                    kind,
                    "resolved",
                    json!({"outcome":"approval","decision":decision}),
                ));
            }
        }
        for kind in [
            "command_execution_approval",
            "file_change_approval",
            "user_input",
        ] {
            for reason in [
                "upstream_cleared",
                "run_generation_changed",
                "server_epoch_changed",
                "turn_terminal",
                "file_change_snapshot_changed",
                "file_change_artifact_unavailable",
            ] {
                cases.push((kind, "stale", json!({"outcome":"stale","reason":reason})));
            }
            if journal {
                cases.push((
                    kind,
                    "stale",
                    json!({"outcome":"stale","reason":"terminal_turn"}),
                ));
            }
        }
        cases.push(("user_input", "resolved", json!({"outcome":"answered","contained_secret":true,"answer_digest":null,"answer_count":1,"resolution_receipt_id":"018faaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee"})));
        cases.push(("user_input", "resolved", json!({"outcome":"answered","contained_secret":false,"answer_digest":"a".repeat(64),"answer_count":1,"resolution_receipt_id":null})));
        if journal {
            cases.push(("user_input", "resolved", json!({"outcome":"answered","contained_secret":false,"answer_digest":"a".repeat(64),"answer_count":1,"resolution_receipt_id":"018faaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee"})));
        } else {
            cases.push((
                "unsupported_request",
                "resolved",
                json!({"outcome":"method_not_found","reply_code":-32601}),
            ));
        }
        cases
    }

    pub(crate) fn malformed(valid: &Value) -> Vec<Value> {
        let mut cases = vec![
            Value::Null,
            json!(false),
            json!(7),
            json!("approved"),
            json!([]),
            json!({}),
            json!({"outcome":"approved"}),
        ];
        for outcome in [
            Value::Null,
            json!(true),
            json!(7),
            json!([]),
            json!({}),
            json!("unknown"),
        ] {
            let mut candidate = valid.clone();
            candidate["outcome"] = outcome;
            cases.push(candidate);
        }
        let object = valid.as_object().unwrap();
        for key in object.keys() {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(key);
            cases.push(missing);
        }
        let mut extra = valid.clone();
        extra["extra"] = json!(true);
        cases.push(extra);
        let fields: Vec<(&str, Vec<Value>)> = match valid["outcome"].as_str().unwrap() {
            "approval" => vec![(
                "decision",
                vec![
                    Value::Null,
                    json!(false),
                    json!([]),
                    json!({}),
                    json!(1),
                    json!("accept"),
                    json!(""),
                ],
            )],
            "stale" => vec![(
                "reason",
                vec![
                    Value::Null,
                    json!(false),
                    json!([]),
                    json!({}),
                    json!(1),
                    json!("other"),
                    json!(""),
                ],
            )],
            "method_not_found" => vec![(
                "reply_code",
                vec![
                    Value::Null,
                    json!(false),
                    json!([]),
                    json!({}),
                    json!("-32601"),
                    json!(-32600),
                ],
            )],
            "answered" => vec![
                (
                    "contained_secret",
                    vec![Value::Null, json!("false"), json!(1), json!([]), json!({})],
                ),
                (
                    "answer_count",
                    vec![
                        Value::Null,
                        json!(true),
                        json!("1"),
                        json!(0),
                        json!(-1),
                        json!(1.5),
                        json!(9_007_199_254_740_992_u64),
                    ],
                ),
                (
                    "answer_digest",
                    if valid["contained_secret"] == true {
                        vec![json!("a".repeat(64)), json!(true)]
                    } else {
                        vec![
                            Value::Null,
                            json!(true),
                            json!("A".repeat(64)),
                            json!("a".repeat(63)),
                            json!("g".repeat(64)),
                            json!("a".repeat(65)),
                            json!(format!("sha256:{}", "a".repeat(64))),
                        ]
                    },
                ),
                (
                    "resolution_receipt_id",
                    vec![
                        json!(false),
                        json!(7),
                        json!([]),
                        json!({}),
                        json!("018FAAAA-BBBB-7CCC-8DDD-EEEEEEEEEEEE"),
                        json!("018faaaabbbb7ccc8dddeeeeeeeeeeee"),
                        json!("{018faaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee}"),
                        json!("urn:uuid:018faaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee"),
                        json!("018faaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
                        json!("018faaaa-bbbb-7ccc-0ddd-eeeeeeeeeeee"),
                    ],
                ),
            ],
            _ => unreachable!(),
        };
        for (field, inputs) in fields {
            for input in inputs {
                let mut case = valid.clone();
                case[field] = input;
                cases.push(case);
            }
        }
        if valid["contained_secret"] == true {
            let mut case = valid.clone();
            case["resolution_receipt_id"] = Value::Null;
            cases.push(case);
        }
        cases
    }

    pub(crate) fn invalid_dates() -> Vec<Value> {
        vec![
            Value::Null,
            json!(true),
            json!(0),
            json!([]),
            json!({}),
            json!(""),
            json!("not-a-date"),
            json!("2026-10-26T00:00:60Z"),
            json!("2016-12-31T23:58:60Z"),
            json!("2016-12-30T23:59:60Z"),
            json!("2016-11-30T23:59:60Z"),
            json!("2016-12-31T23:59:60+01:00"),
            json!("2017-01-01T08:59:60+08:00"),
            json!("2026-02-30T00:00:00Z"),
            json!("2025-02-29T00:00:00Z"),
            json!("2026-13-01T00:00:00Z"),
            json!("2026-01-00T00:00:00Z"),
            json!("2026-01-01T24:00:00Z"),
            json!("2026-01-01T00:60:00Z"),
            json!("2026-01-01T00:00:61Z"),
            json!("2026-01-01"),
            json!("2026-01-01 00:00:00Z"),
            json!("2026-01-01T00:00:00"),
            json!("2026-01-01T00:00:00.Z"),
            json!("2026-01-01T00:00:00+24:00"),
            json!("2026-01-01T00:00:00+00:60"),
            json!("2026-01-01T00:00:00+0900"),
            json!("2026-01-01T00:00:00 +09:00"),
            json!("2026-01-01T00:00:00Zsuffix"),
        ]
    }
}
