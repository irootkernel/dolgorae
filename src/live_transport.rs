//! Isolated TASK-025 transport probe. This is not the production Primary bridge.

use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
pub use crate::primary_tool::TRANSPORT_BOUND_FIELDS;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const REQUIRED_PROBE_SCENARIOS: [&str; 12] = [
    "source_binding",
    "model_identity_injection",
    "same_call_retry",
    "different_input_conflict",
    "concurrent_call_isolation",
    "cancellation",
    "bounded_wait_expiry",
    "connection_loss",
    "bridge_restart",
    "stale_generation",
    "credential_canary",
    "shared_identity_ambiguity",
];

pub const CANARY_FRAGMENTS: [&str; 6] = [
    "capability",
    "BEGIN PRIVATE",
    "/private/sockets/",
    "sqlite",
    "raw-frame",
    "child-controller",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportCandidate {
    PrivateMcp,
    NativeRunBound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLaneKind {
    SharedProfileServer,
    DedicatedLane,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioVerdict {
    Proved,
    Failed,
    Ambiguous,
}

/// Isolated TASK-025 probe helper. It is not production source authentication.
/// The live bridge must prove Run/Thread/Turn/generation membership before
/// constructing an equivalent call context.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedBinding {
    pub session_id: Uuid,
    pub source_run_id: Uuid,
    pub source_turn_id: String,
    pub source_tool_call_id: String,
    pub worker_generation: u64,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeTurnContext {
    pub session_id: Uuid,
    pub run_id: Uuid,
    pub turn_id: String,
    pub tool_item_id: String,
    pub worker_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpCallContext {
    pub process_session_id: Option<Uuid>,
    pub process_run_id: Option<Uuid>,
    pub process_generation: u64,
    pub lane: ExecutionLaneKind,
    pub jsonrpc_id: Option<String>,
    pub connection_id: Option<String>,
    pub meta_turn_id: Option<String>,
    pub meta_tool_call_id: Option<String>,
    pub meta_idempotency_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReceipt {
    pub receipt_id: String,
    pub payload_sha256: String,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProbeOutcome {
    Accepted(ProbeReceipt),
    Replay(ProbeReceipt),
    WaitExpired { remaining_deadline_ms: u64 },
    InterruptedUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioObservation {
    pub scenario: String,
    pub candidate: TransportCandidate,
    pub lane: ExecutionLaneKind,
    pub verdict: ScenarioVerdict,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportSelection {
    pub status: String,
    pub selected: Option<TransportCandidate>,
    pub reason: String,
}

#[derive(Default)]
pub struct ProbeLedger {
    receipts: BTreeMap<String, (String, ProbeReceipt)>,
}

impl TrustedBinding {
    pub fn validate(&self) -> Result<(), MachineError> {
        checked(&self.source_turn_id, 256, "source_turn_id")?;
        checked(&self.source_tool_call_id, 256, "source_tool_call_id")?;
        checked(&self.idempotency_key, 256, "idempotency_key")
    }

    fn semantic_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.session_id,
            self.source_run_id,
            self.source_turn_id,
            self.source_tool_call_id,
            self.idempotency_key
        )
    }
}

impl NativeTurnContext {
    pub fn binding(&self, idempotency_key: &str) -> Result<TrustedBinding, MachineError> {
        let binding = TrustedBinding {
            session_id: self.session_id,
            source_run_id: self.run_id,
            source_turn_id: self.turn_id.clone(),
            source_tool_call_id: self.tool_item_id.clone(),
            worker_generation: self.worker_generation,
            idempotency_key: idempotency_key.to_owned(),
        };
        binding.validate()?;
        Ok(binding)
    }
}

impl McpCallContext {
    pub fn binding(&self) -> Result<TrustedBinding, MachineError> {
        if self.lane == ExecutionLaneKind::SharedProfileServer
            && (self.process_run_id.is_none()
                || self.connection_id.is_some()
                || self.jsonrpc_id.is_some())
            && (self.meta_turn_id.is_none() || self.meta_tool_call_id.is_none())
        {
            return Err(identity_error(
                "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                "shared MCP identity is ambiguous without a host-controlled Turn/call carrier",
            ));
        }
        let Some(session_id) = self.process_session_id else {
            return Err(identity_error(
                "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                "MCP process is not bound to a session",
            ));
        };
        let Some(source_run_id) = self.process_run_id else {
            return Err(identity_error(
                "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                "MCP process is not bound to a Run",
            ));
        };
        let Some(source_turn_id) = self.meta_turn_id.clone() else {
            return Err(identity_error(
                "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                "Dedicated Lane isolation does not prove Turn identity",
            ));
        };
        let Some(source_tool_call_id) = self.meta_tool_call_id.clone() else {
            return Err(identity_error(
                "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                "Dedicated Lane isolation does not prove tool-call identity",
            ));
        };
        let Some(idempotency_key) = self.meta_idempotency_key.clone() else {
            return Err(identity_error(
                "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                "the model cannot supply the idempotency key",
            ));
        };
        let binding = TrustedBinding {
            session_id,
            source_run_id,
            source_turn_id,
            source_tool_call_id,
            worker_generation: self.process_generation,
            idempotency_key,
        };
        binding.validate()?;
        Ok(binding)
    }
}

pub fn reject_model_identity(payload: &Value) -> Result<(), MachineError> {
    let Some(object) = payload.as_object() else {
        return Err(MachineError::invalid_argument(
            "tool_payload",
            "payload must be a JSON object",
        ));
    };
    if object
        .keys()
        .any(|key| TRANSPORT_BOUND_FIELDS.contains(&key.as_str()))
    {
        return Err(MachineError::invalid_argument(
            "tool_payload",
            "transport-bound identity fields are not model-controlled",
        ));
    }
    Ok(())
}

pub fn reject_canary(value: &Value) -> Result<(), MachineError> {
    let rendered = value.to_string();
    if CANARY_FRAGMENTS
        .iter()
        .any(|fragment| rendered.contains(fragment))
    {
        return Err(identity_error(
            "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
            "probe result would disclose a credential, socket, database, or raw frame",
        ));
    }
    Ok(())
}

pub fn payload_sha256(payload: &Value) -> Result<String, MachineError> {
    let text = serde_json::to_string(payload).map_err(internal_payload)?;
    let canonical =
        canonicalize(&parse(&text).map_err(internal_payload)?).map_err(internal_payload)?;
    Ok(sha256_hex(&canonical))
}

fn internal_payload(error: impl ToString) -> MachineError {
    MachineError::new(
        "INTERNAL",
        "payload could not be canonicalized",
        false,
        json!({"reason": error.to_string()}),
    )
}

impl ProbeLedger {
    pub fn dispatch(
        &mut self,
        binding: &TrustedBinding,
        payload: &Value,
        current_generation: u64,
        invoke: ProbeInvoke,
    ) -> Result<ProbeOutcome, MachineError> {
        binding.validate()?;
        reject_model_identity(payload)?;
        if binding.worker_generation != current_generation {
            return Err(identity_error(
                "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                "stale worker generation cannot mint a new semantic identity",
            ));
        }
        let digest = payload_sha256(payload)?;
        let key = binding.semantic_key();
        if let Some((stored_digest, receipt)) = self.receipts.get(&key) {
            if stored_digest == &digest {
                return Ok(ProbeOutcome::Replay(receipt.clone()));
            }
            return Err(identity_error(
                "ORCHESTRATION_IDEMPOTENCY_CONFLICT",
                "the same call identity was reused with a different payload",
            ));
        }
        match invoke {
            ProbeInvoke::Complete { result } => {
                reject_canary(&result)?;
                let receipt = ProbeReceipt {
                    receipt_id: format!("receipt-{digest}"),
                    payload_sha256: digest.clone(),
                    state: "accepted".to_owned(),
                };
                self.receipts.insert(key, (digest, receipt.clone()));
                Ok(ProbeOutcome::Accepted(receipt))
            }
            ProbeInvoke::WaitExpire {
                remaining_deadline_ms,
            } => Ok(ProbeOutcome::WaitExpired {
                remaining_deadline_ms,
            }),
            ProbeInvoke::DisconnectWithoutReceipt => {
                let receipt = ProbeReceipt {
                    receipt_id: format!("receipt-{digest}"),
                    payload_sha256: digest.clone(),
                    state: "interrupted_unknown".to_owned(),
                };
                self.receipts.insert(key, (digest, receipt));
                Ok(ProbeOutcome::InterruptedUnknown)
            }
            ProbeInvoke::Cancel => {
                let receipt = ProbeReceipt {
                    receipt_id: format!("receipt-{digest}"),
                    payload_sha256: digest.clone(),
                    state: "cancelled".to_owned(),
                };
                self.receipts.insert(key, (digest, receipt.clone()));
                Ok(ProbeOutcome::Accepted(receipt))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProbeInvoke {
    Complete { result: Value },
    WaitExpire { remaining_deadline_ms: u64 },
    DisconnectWithoutReceipt,
    Cancel,
}

pub fn bind_collaboration_stub(
    binding: &TrustedBinding,
    payload: &Value,
) -> Result<Value, MachineError> {
    binding.validate()?;
    reject_model_identity(payload)?;
    Ok(json!({
        "operation": "collaboration_stub_result",
        "state": "inert",
        "mailbox": false,
        "scheduler": false,
    }))
}

pub fn select_transport(observations: &[ScenarioObservation]) -> TransportSelection {
    let names = observations
        .iter()
        .map(|observation| observation.scenario.as_str())
        .collect::<BTreeSet<_>>();
    if REQUIRED_PROBE_SCENARIOS
        .iter()
        .any(|scenario| !names.contains(scenario))
    {
        return TransportSelection {
            status: "unselected".to_owned(),
            selected: None,
            reason: "required probe scenarios are incomplete".to_owned(),
        };
    }
    let native_ok = candidate_proved(observations, TransportCandidate::NativeRunBound);
    let mcp_ok = candidate_proved(observations, TransportCandidate::PrivateMcp);
    match (native_ok, mcp_ok) {
        (true, false) => TransportSelection {
            status: "fixture_proved".to_owned(),
            selected: Some(TransportCandidate::NativeRunBound),
            reason: "native worker Turn/call binding proved in fixtures; live pin evidence remains required".to_owned(),
        },
        (false, true) => TransportSelection {
            status: "fixture_proved".to_owned(),
            selected: Some(TransportCandidate::PrivateMcp),
            reason: "MCP host-controlled Turn/call carrier proved in fixtures; live pin evidence remains required".to_owned(),
        },
        (true, true) => TransportSelection {
            status: "fixture_proved".to_owned(),
            selected: Some(TransportCandidate::NativeRunBound),
            reason: "both candidates proved in fixtures; native shared-server binding is preferred".to_owned(),
        },
        (false, false) => TransportSelection {
            status: "unselected".to_owned(),
            selected: None,
            reason: "no candidate proved the required transport contract".to_owned(),
        },
    }
}

fn candidate_proved(observations: &[ScenarioObservation], candidate: TransportCandidate) -> bool {
    REQUIRED_PROBE_SCENARIOS.iter().all(|scenario| {
        observations.iter().any(|observation| {
            observation.scenario == *scenario
                && observation.candidate == candidate
                && observation.verdict == expected_verdict(scenario, observation.lane)
        })
    })
}

fn expected_verdict(scenario: &str, lane: ExecutionLaneKind) -> ScenarioVerdict {
    match (scenario, lane) {
        ("shared_identity_ambiguity", ExecutionLaneKind::SharedProfileServer) => {
            ScenarioVerdict::Ambiguous
        }
        ("shared_identity_ambiguity", ExecutionLaneKind::DedicatedLane) => ScenarioVerdict::Proved,
        _ => ScenarioVerdict::Proved,
    }
}

fn checked(value: &str, maximum: usize, field: &str) -> Result<(), MachineError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(MachineError::invalid_argument(
            field,
            "value must be nonempty, bounded, and printable",
        ));
    }
    Ok(())
}

fn identity_error(code: &str, message: &str) -> MachineError {
    MachineError::new(code, message, false, json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::new_uuid_v7;

    fn native(generation: u64) -> NativeTurnContext {
        NativeTurnContext {
            session_id: new_uuid_v7(),
            run_id: new_uuid_v7(),
            turn_id: "turn-1".to_owned(),
            tool_item_id: "call-1".to_owned(),
            worker_generation: generation,
        }
    }

    fn payload() -> Value {
        json!({"operation":"probe","nonce":"alpha"})
    }

    #[test]
    fn native_binding_ignores_model_payload_and_replays_the_same_call() {
        let context = native(3);
        let binding = context.binding("idem-1").unwrap();
        let mut ledger = ProbeLedger::default();
        let first = ledger
            .dispatch(
                &binding,
                &payload(),
                3,
                ProbeInvoke::Complete {
                    result: json!({"ok":true}),
                },
            )
            .unwrap();
        let replay = ledger
            .dispatch(
                &binding,
                &payload(),
                3,
                ProbeInvoke::Complete {
                    result: json!({"ok":true}),
                },
            )
            .unwrap();
        match (first, replay) {
            (ProbeOutcome::Accepted(original), ProbeOutcome::Replay(again)) => {
                assert_eq!(original, again);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cancel_records_a_cancelled_receipt_and_replays_it() {
        let context = native(1);
        let binding = context.binding("idem-cancel").unwrap();
        let mut other = context.clone();
        other.tool_item_id = "call-2".to_owned();
        let isolated = other.binding("idem-other").unwrap();
        let mut ledger = ProbeLedger::default();
        let cancelled = ledger
            .dispatch(&binding, &payload(), 1, ProbeInvoke::Cancel)
            .unwrap();
        assert!(matches!(
            cancelled,
            ProbeOutcome::Accepted(receipt) if receipt.state == "cancelled"
        ));
        let replayed = ledger
            .dispatch(&binding, &payload(), 1, ProbeInvoke::Cancel)
            .unwrap();
        assert!(matches!(
            replayed,
            ProbeOutcome::Replay(receipt) if receipt.state == "cancelled"
        ));
        let concurrent = ledger
            .dispatch(
                &isolated,
                &payload(),
                1,
                ProbeInvoke::Complete {
                    result: json!({"ok": true}),
                },
            )
            .unwrap();
        assert!(matches!(concurrent, ProbeOutcome::Accepted(_)));
    }

    #[test]
    fn model_injected_identity_is_rejected() {
        let context = native(1);
        let binding = context.binding("idem-1").unwrap();
        let mut ledger = ProbeLedger::default();
        let error = ledger
            .dispatch(
                &binding,
                &json!({
                    "operation":"probe",
                    "source_turn_id":"forged",
                    "nonce":"alpha"
                }),
                1,
                ProbeInvoke::Complete {
                    result: json!({"ok":true}),
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENT");
    }

    #[test]
    fn different_input_conflicts_and_concurrent_calls_stay_isolated() {
        let context = native(1);
        let first = context.binding("idem-1").unwrap();
        let mut other = context.clone();
        other.tool_item_id = "call-2".to_owned();
        let second = other.binding("idem-2").unwrap();
        let mut ledger = ProbeLedger::default();
        ledger
            .dispatch(
                &first,
                &payload(),
                1,
                ProbeInvoke::Complete {
                    result: json!({"ok":true}),
                },
            )
            .unwrap();
        assert_eq!(
            ledger
                .dispatch(
                    &first,
                    &json!({"operation":"probe","nonce":"beta"}),
                    1,
                    ProbeInvoke::Complete {
                        result: json!({"ok":true}),
                    },
                )
                .unwrap_err()
                .code,
            "ORCHESTRATION_IDEMPOTENCY_CONFLICT"
        );
        let concurrent = ledger
            .dispatch(
                &second,
                &json!({"operation":"probe","nonce":"beta"}),
                1,
                ProbeInvoke::Complete {
                    result: json!({"ok":true}),
                },
            )
            .unwrap();
        assert!(matches!(concurrent, ProbeOutcome::Accepted(_)));
    }

    #[test]
    fn cross_run_or_turn_substitution_is_not_replay() {
        let context = native(1);
        let original = context.binding("idem-1").unwrap();
        let mut other_turn = context.clone();
        other_turn.turn_id = "turn-2".to_owned();
        let substituted_turn = other_turn.binding("idem-1").unwrap();
        let mut other_run = context.clone();
        other_run.run_id = new_uuid_v7();
        let substituted_run = other_run.binding("idem-1").unwrap();
        let mut ledger = ProbeLedger::default();
        ledger
            .dispatch(
                &original,
                &payload(),
                1,
                ProbeInvoke::Complete {
                    result: json!({"ok": true}),
                },
            )
            .unwrap();
        assert!(matches!(
            ledger
                .dispatch(
                    &substituted_turn,
                    &payload(),
                    1,
                    ProbeInvoke::Complete {
                        result: json!({"ok": true}),
                    },
                )
                .unwrap(),
            ProbeOutcome::Accepted(_)
        ));
        assert!(matches!(
            ledger
                .dispatch(
                    &substituted_run,
                    &payload(),
                    1,
                    ProbeInvoke::Complete {
                        result: json!({"ok": true}),
                    },
                )
                .unwrap(),
            ProbeOutcome::Accepted(_)
        ));
    }

    #[test]
    fn wait_expiry_does_not_cancel_and_disconnect_without_receipt_is_unknown() {
        let context = native(1);
        let binding = context.binding("idem-wait").unwrap();
        let mut ledger = ProbeLedger::default();
        let expired = ledger
            .dispatch(
                &binding,
                &payload(),
                1,
                ProbeInvoke::WaitExpire {
                    remaining_deadline_ms: 4_000,
                },
            )
            .unwrap();
        assert!(matches!(
            expired,
            ProbeOutcome::WaitExpired {
                remaining_deadline_ms: 4_000
            }
        ));
        assert!(ledger.receipts.is_empty());
        let continued = ledger
            .dispatch(
                &binding,
                &payload(),
                1,
                ProbeInvoke::Complete {
                    result: json!({"ok": true}),
                },
            )
            .unwrap();
        assert!(matches!(continued, ProbeOutcome::Accepted(_)));
    }

    #[test]
    fn disconnect_without_receipt_persists_unknown_and_blocks_later_mint() {
        let context = native(1);
        let binding = context.binding("idem-lost").unwrap();
        let mut ledger = ProbeLedger::default();
        let lost = ledger
            .dispatch(
                &binding,
                &payload(),
                1,
                ProbeInvoke::DisconnectWithoutReceipt,
            )
            .unwrap();
        assert!(matches!(lost, ProbeOutcome::InterruptedUnknown));
        let replayed = ledger
            .dispatch(
                &binding,
                &payload(),
                1,
                ProbeInvoke::Complete {
                    result: json!({"ok": true}),
                },
            )
            .unwrap();
        assert!(matches!(
            replayed,
            ProbeOutcome::Replay(receipt) if receipt.state == "interrupted_unknown"
        ));
        let cancelled = ledger
            .dispatch(&binding, &payload(), 1, ProbeInvoke::Cancel)
            .unwrap();
        assert!(matches!(
            cancelled,
            ProbeOutcome::Replay(receipt) if receipt.state == "interrupted_unknown"
        ));
    }

    #[test]
    fn restart_replays_durable_receipt_and_stale_generation_cannot_mint_identity() {
        let mut context = native(1);
        let binding = context.binding("idem-1").unwrap();
        let mut ledger = ProbeLedger::default();
        ledger
            .dispatch(
                &binding,
                &payload(),
                1,
                ProbeInvoke::Complete {
                    result: json!({"ok":true}),
                },
            )
            .unwrap();
        context.worker_generation = 2;
        let restarted = context.binding("idem-1").unwrap();
        let replay = ledger
            .dispatch(
                &restarted,
                &payload(),
                2,
                ProbeInvoke::Complete {
                    result: json!({"ok":true}),
                },
            )
            .unwrap();
        assert!(matches!(replay, ProbeOutcome::Replay(_)));
        assert_eq!(
            ledger
                .dispatch(
                    &binding,
                    &payload(),
                    2,
                    ProbeInvoke::Complete {
                        result: json!({"ok":true}),
                    },
                )
                .unwrap_err()
                .code,
            "ORCHESTRATION_TRANSPORT_UNAVAILABLE"
        );
    }

    #[test]
    fn canaries_and_collaboration_stub_keep_secrets_and_mailbox_inert() {
        let context = native(1);
        let binding = context.binding("idem-1").unwrap();
        let mut ledger = ProbeLedger::default();
        assert_eq!(
            ledger
                .dispatch(
                    &binding,
                    &payload(),
                    1,
                    ProbeInvoke::Complete {
                        result: json!({"socket":"/private/sockets/worker.sock"}),
                    },
                )
                .unwrap_err()
                .code,
            "ORCHESTRATION_TRANSPORT_UNAVAILABLE"
        );
        let stub =
            bind_collaboration_stub(&binding, &json!({"operation":"consult","nonce":"x"})).unwrap();
        assert_eq!(stub["state"], "inert");
        assert_eq!(stub["mailbox"], false);
        assert_eq!(stub["scheduler"], false);
        assert!(
            bind_collaboration_stub(
                &binding,
                &json!({"operation":"consult","source_turn_id":"forged"})
            )
            .is_err()
        );
    }

    #[test]
    fn shared_mcp_without_meta_is_ambiguous_and_dedicated_lane_still_needs_turn_call() {
        let shared = McpCallContext {
            process_session_id: Some(new_uuid_v7()),
            process_run_id: Some(new_uuid_v7()),
            process_generation: 1,
            lane: ExecutionLaneKind::SharedProfileServer,
            jsonrpc_id: Some("1".to_owned()),
            connection_id: Some("conn".to_owned()),
            meta_turn_id: None,
            meta_tool_call_id: None,
            meta_idempotency_key: None,
        };
        assert_eq!(
            shared.binding().unwrap_err().code,
            "ORCHESTRATION_TRANSPORT_UNAVAILABLE"
        );
        let dedicated = McpCallContext {
            lane: ExecutionLaneKind::DedicatedLane,
            jsonrpc_id: None,
            connection_id: None,
            ..shared
        };
        assert!(
            dedicated
                .binding()
                .unwrap_err()
                .message
                .contains("does not prove Turn identity")
        );
    }

    #[test]
    fn mcp_with_host_meta_binds_and_fixture_selection_prefers_native() {
        let session = new_uuid_v7();
        let run = new_uuid_v7();
        let mcp = McpCallContext {
            process_session_id: Some(session),
            process_run_id: Some(run),
            process_generation: 4,
            lane: ExecutionLaneKind::DedicatedLane,
            jsonrpc_id: Some("ignored".to_owned()),
            connection_id: Some("ignored".to_owned()),
            meta_turn_id: Some("turn-9".to_owned()),
            meta_tool_call_id: Some("call-9".to_owned()),
            meta_idempotency_key: Some("idem-9".to_owned()),
        }
        .binding()
        .unwrap();
        assert_eq!(mcp.source_turn_id, "turn-9");
        assert_eq!(mcp.worker_generation, 4);

        let native_obs = REQUIRED_PROBE_SCENARIOS
            .iter()
            .map(|scenario| ScenarioObservation {
                scenario: (*scenario).to_owned(),
                candidate: TransportCandidate::NativeRunBound,
                lane: if *scenario == "shared_identity_ambiguity" {
                    ExecutionLaneKind::DedicatedLane
                } else {
                    ExecutionLaneKind::SharedProfileServer
                },
                verdict: ScenarioVerdict::Proved,
            })
            .collect::<Vec<_>>();
        let mut mcp_obs = native_obs.clone();
        for observation in &mut mcp_obs {
            observation.candidate = TransportCandidate::PrivateMcp;
            if observation.scenario == "shared_identity_ambiguity" {
                observation.lane = ExecutionLaneKind::SharedProfileServer;
                observation.verdict = ScenarioVerdict::Ambiguous;
            } else {
                observation.verdict = ScenarioVerdict::Failed;
            }
        }
        let mut all = native_obs;
        all.extend(mcp_obs);
        let selection = select_transport(&all);
        assert_eq!(selection.status, "fixture_proved");
        assert_eq!(selection.selected, Some(TransportCandidate::NativeRunBound));
    }

    #[test]
    fn pinned_selection_artifact_records_local_codex_live_evidence() {
        let artifact: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-live-transport-selection-v1.json"
        ))
        .unwrap();
        assert_eq!(artifact["codex_pin"], "0.155.1");
        assert_eq!(artifact["status"], "selected");
        assert_eq!(artifact["selected"], "native_run_bound");
        assert_eq!(artifact["live_evidence"], "recorded");
        assert!(
            artifact["registration_mechanism"]
                .as_str()
                .unwrap()
                .contains("item/tool/call")
        );
        let scenarios = artifact["required_scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(scenarios, REQUIRED_PROBE_SCENARIOS);
    }
}
