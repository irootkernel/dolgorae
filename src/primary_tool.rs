//! Checked model-facing contract for the private Primary orchestration tool.

use crate::machine::MachineError;
use serde_json::Value;
use uuid::Uuid;

pub const PRIMARY_TOOL_NAME: &str = "dolgorae_orchestration";
pub const TRANSPORT_BOUND_FIELDS: [&str; 7] = [
    "orchestrated_session_id",
    "source_primary_run_id",
    "source_turn_id",
    "source_tool_call_id",
    "controller_principal",
    "root_priority",
    "idempotency_key",
];

const REQUEST_DEFS: [&str; 9] = [
    "request_specialist_request",
    "await_operations_request",
    "list_specialists_request",
    "assign_task_request",
    "await_tasks_request",
    "collect_results_request",
    "read_result_request",
    "cancel_task_request",
    "release_request",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimaryCallContext {
    pub session_id: Uuid,
    pub source_run_id: Uuid,
    pub source_turn_id: String,
    pub source_tool_call_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug)]
pub struct PrimaryToolContract {
    tool_spec: Value,
}

impl PrimaryToolContract {
    pub fn load() -> Result<Self, MachineError> {
        let source: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-orchestration-tool-v1.schema.json"
        ))
        .map_err(|_| contract_invalid("Primary tool schema is invalid"))?;
        let defs = source
            .get("$defs")
            .cloned()
            .ok_or_else(|| contract_invalid("Primary tool schema is incomplete"))?;
        let one_of = REQUEST_DEFS
            .iter()
            .map(|name| serde_json::json!({"$ref":format!("#/$defs/{name}")}))
            .collect::<Vec<_>>();
        Ok(Self {
            tool_spec: serde_json::json!({
                "type":"function",
                "name":PRIMARY_TOOL_NAME,
                "description":"Operate the current Dolgorae Orchestrated Session through checked private requests.",
                "inputSchema":{
                    "$schema":"https://json-schema.org/draft/2020-12/schema",
                    "oneOf":one_of,
                    "$defs":defs,
                },
            }),
        })
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        PRIMARY_TOOL_NAME
    }

    #[must_use]
    pub fn tool_spec(&self) -> Value {
        self.tool_spec.clone()
    }

    pub fn reject_model_identity(&self, payload: &Value) -> Result<(), MachineError> {
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

    #[must_use]
    pub fn error_result(&self, error: &MachineError) -> Value {
        let (code, retryable, message) = match error.code.as_str() {
            "IDEMPOTENCY_CONFLICT" => (
                "ORCHESTRATION_IDEMPOTENCY_CONFLICT",
                false,
                "the tool call identity was already used with different input",
            ),
            "SPECIALIST_POLICY_DENIED" | "SPECIALIST_REQUEST_DENIED" => (
                error.code.as_str(),
                false,
                "the Specialist Policy denied the request",
            ),
            "SPECIALIST_NOT_MEMBER" => (
                "SPECIALIST_NOT_ACTIVE",
                false,
                "the requested Specialist is not active in this Session",
            ),
            "SPECIALIST_TASK_NOT_FOUND" => (
                "SPECIALIST_TASK_NOT_FOUND",
                false,
                "the requested Specialist task was not found",
            ),
            "SPECIALIST_RESULT_UNREADABLE" => (
                "SPECIALIST_RESULT_UNREADABLE",
                false,
                "the requested Specialist result is not readable",
            ),
            "LIVE_POLICY_UNSUPPORTED" => (
                "LIVE_POLICY_UNSUPPORTED",
                false,
                "the Specialist Policy is not supported by the live provider",
            ),
            "SPECIALIST_WRITER_CONFLICT" | "WRITER_BUSY" => (
                "SPECIALIST_WRITER_CONFLICT",
                false,
                "writer authority is not currently available",
            ),
            "RUN_STATE_CONFLICT" => (
                "RUN_STATE_CONFLICT",
                false,
                "the current Session state conflicts with the request",
            ),
            "ORCHESTRATION_OPERATION_UNAVAILABLE" => (
                "ORCHESTRATION_NOT_AVAILABLE",
                false,
                "the requested operation is not available in this provider slice",
            ),
            "INVALID_ARGUMENT" => (
                "ORCHESTRATION_NOT_AVAILABLE",
                false,
                "the request does not match the checked orchestration contract",
            ),
            _ => (
                "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                true,
                "the orchestration service did not produce a safe final outcome",
            ),
        };
        serde_json::json!({
            "operation":"orchestration_error",
            "code":code,
            "retryable":retryable,
            "message":message,
        })
    }
}

fn contract_invalid(reason: &'static str) -> MachineError {
    MachineError::new(
        "ORCHESTRATION_CONTRACT_INVALID",
        reason,
        false,
        serde_json::json!({}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_excludes_transport_identity_and_maps_safe_errors() {
        let contract = PrimaryToolContract::load().unwrap();
        assert_eq!(contract.tool_spec()["name"], PRIMARY_TOOL_NAME);
        assert_eq!(
            contract.tool_spec()["inputSchema"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            9
        );
        for field in TRANSPORT_BOUND_FIELDS {
            assert!(
                !contract.tool_spec()["inputSchema"]
                    .to_string()
                    .contains(field)
            );
        }
        let error = contract
            .reject_model_identity(&serde_json::json!({
                "operation":"list_specialists",
                "source_primary_run_id":Uuid::now_v7(),
            }))
            .unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENT");
        let result = contract.error_result(&error);
        assert_eq!(result["code"], "ORCHESTRATION_NOT_AVAILABLE");
        assert!(result.get("details").is_none());
    }

    #[test]
    fn contract_advertises_specialist_result_reader() {
        let spec = PrimaryToolContract::load().unwrap().tool_spec();
        let schema = &spec["inputSchema"];
        assert!(
            schema["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .any(|request| request["$ref"] == "#/$defs/read_result_request")
        );
        assert_eq!(
            schema["$defs"]["read_result_request"]["properties"]["operation"]["const"],
            "read_specialist_result"
        );
    }

    #[test]
    fn unreadable_result_error_preserves_nonretryable_contract() {
        let contract = PrimaryToolContract::load().unwrap();
        let error = MachineError::new(
            "SPECIALIST_RESULT_UNREADABLE",
            "private-result-canary",
            false,
            serde_json::json!({"artifact_path":"private-result-canary"}),
        );
        let result = contract.error_result(&error);
        assert_eq!(result["operation"], "orchestration_error");
        assert_eq!(result["code"], "SPECIALIST_RESULT_UNREADABLE");
        assert_eq!(result["retryable"], false);
        assert!(result.get("details").is_none());
        assert!(!result.to_string().contains("private-result-canary"));
    }

    #[test]
    fn unsupported_live_policy_error_preserves_nonretryable_contract() {
        let error = MachineError::new(
            "LIVE_POLICY_UNSUPPORTED",
            "private-policy-canary",
            false,
            serde_json::json!({"policy_name":"private-policy-canary"}),
        );
        let result = PrimaryToolContract::load().unwrap().error_result(&error);
        assert_eq!(result["operation"], "orchestration_error");
        assert_eq!(result["code"], "LIVE_POLICY_UNSUPPORTED");
        assert_eq!(result["retryable"], false);
        assert!(result.get("details").is_none());
        assert!(!result.to_string().contains("private-policy-canary"));
    }
}
