//! Transport-neutral durable Interaction observations and protected response bounds.
use crate::domain::RunLifecycle;
use crate::machine::MachineError;
use crate::snapshot::RunSnapshot;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) const MAX_RESPONSE_BYTES: usize = 1_048_576;
pub(crate) const MAX_PAYLOAD_BYTES: usize = 8_388_608;

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
