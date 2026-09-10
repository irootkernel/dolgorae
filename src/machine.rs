use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use uuid::{Timestamp, Uuid};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuccessEnvelope {
    pub schema_version: u32,
    pub ok: bool,
    pub command: String,
    pub invocation_id: Uuid,
    pub data: Value,
}

impl SuccessEnvelope {
    #[must_use]
    pub fn new(command: impl Into<String>, data: Value) -> Self {
        Self {
            schema_version: 2,
            ok: true,
            command: command.into(),
            invocation_id: new_uuid_v7(),
            data,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Value,
}

impl From<crate::paths::DolgoraeHomeError> for MachineError {
    fn from(error: crate::paths::DolgoraeHomeError) -> Self {
        let (path, reason) = error.into_parts();
        Self::runtime_path_invalid(path, reason)
    }
}

impl MachineError {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
        details: Value,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            details,
        }
    }

    #[must_use]
    pub fn invalid_argument(argument: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(
            "INVALID_ARGUMENT",
            "invalid command arguments",
            false,
            serde_json::json!({"argument": argument.into(), "reason": reason.into()}),
        )
    }

    #[must_use]
    pub fn interaction_not_found(
        run_id: Uuid,
        request_id: impl ToString,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            "INTERACTION_NOT_FOUND",
            message,
            false,
            serde_json::json!({"run_id": run_id, "request_id": request_id.to_string()}),
        )
    }

    #[must_use]
    pub fn interaction_full_payload_requires_controller(
        run_id: Uuid,
        operation: impl Into<String>,
    ) -> Self {
        Self::new(
            "INTERACTION_FULL_PAYLOAD_REQUIRES_CONTROLLER",
            "the full interaction payload requires the current Controller",
            false,
            serde_json::json!({"run_id": run_id, "operation": operation.into()}),
        )
    }

    #[must_use]
    pub fn workspace_not_initialized(path: impl AsRef<Path>) -> Self {
        Self::new(
            "WORKSPACE_NOT_INITIALIZED",
            "workspace is not initialized",
            false,
            serde_json::json!({"workspace_path": path_json(path.as_ref())}),
        )
    }

    #[must_use]
    pub fn config_invalid(path: impl AsRef<Path>, reason: impl Into<String>) -> Self {
        Self::new(
            "CONFIG_INVALID",
            "workspace configuration is invalid",
            false,
            serde_json::json!({"path": path_json(path.as_ref()), "reason": reason.into()}),
        )
    }

    #[must_use]
    pub fn profile_config_invalid(path: impl AsRef<Path>, reason: impl Into<String>) -> Self {
        Self::new(
            "PROFILE_CONFIG_INVALID",
            "profile configuration is invalid",
            false,
            serde_json::json!({"path": path_json(path.as_ref()), "reason": reason.into()}),
        )
    }

    #[must_use]
    pub fn initialization_conflict(path: impl AsRef<Path>, reason: impl Into<String>) -> Self {
        Self::new(
            "WORKSPACE_INITIALIZATION_CONFLICT",
            "workspace initialization conflicts with existing state",
            false,
            serde_json::json!({"workspace_path": path_json(path.as_ref()), "reason": reason.into()}),
        )
    }

    #[must_use]
    pub fn runtime_path_invalid(path: impl AsRef<Path>, reason: impl Into<String>) -> Self {
        Self::new(
            "RUNTIME_PATH_INVALID",
            "runtime path is unsafe or unsupported",
            false,
            serde_json::json!({"path": path_json(path.as_ref()), "reason": reason.into()}),
        )
    }

    #[must_use]
    pub fn runtime_path_collision(
        path: impl AsRef<Path>,
        expected: crate::workspace::FileIdentity,
        observed: crate::workspace::FileIdentity,
    ) -> Self {
        Self::new(
            "RUNTIME_PATH_COLLISION",
            "runtime path identity changed",
            false,
            serde_json::json!({
                "path": path_json(path.as_ref()),
                "expected_identity": expected,
                "observed_identity": observed
            }),
        )
    }

    #[must_use]
    pub fn runtime_path_collision_missing(
        path: impl AsRef<Path>,
        expected: crate::workspace::FileIdentity,
        observed: impl Into<String>,
    ) -> Self {
        Self::new(
            "RUNTIME_PATH_COLLISION",
            "runtime path is missing or replaced",
            false,
            serde_json::json!({
                "path": path_json(path.as_ref()),
                "expected_identity": expected,
                "observed_identity": {"error": observed.into()}
            }),
        )
    }

    #[must_use]
    pub fn exit_status(&self) -> u8 {
        exit_status_for(&self.code)
    }
}

/// Current checked error vocabulary, including the v1 codes inherited by v2.
pub(crate) fn registered_errors() -> &'static BTreeMap<String, u8> {
    static ERRORS: OnceLock<BTreeMap<String, u8>> = OnceLock::new();
    ERRORS.get_or_init(|| {
        let mut errors = BTreeMap::new();
        for source in [
            include_str!("../docs/protocol/dolgorae-error-contract-v1.json"),
            include_str!("../docs/protocol/dolgorae-error-contract-v2.json"),
        ] {
            let contract: Value = serde_json::from_str(source).expect("checked error contract");
            for (code, status) in contract["x-exit-status"]
                .as_object()
                .expect("exit statuses")
            {
                let status = u8::try_from(status.as_u64().expect("exit status integer"))
                    .expect("exit status byte");
                if let Some(previous) = errors.insert(code.clone(), status) {
                    assert_eq!(previous, status, "inherited error classification changed");
                }
            }
        }
        errors
    })
}

/// The process exit status one registered error code maps to.
///
/// Kept as a function of the code alone because a caller that only has a code
/// — the hidden worker's fd-3 handoff — must not have to fabricate an error
/// envelope to learn how to exit.
#[must_use]
pub fn exit_status_for(code: &str) -> u8 {
    match code {
        "INVALID_ARGUMENT"
        | "CONTROL_MODE_REQUIRED"
        | "PURPOSE_REQUIRED"
        | "EXECUTION_LANE_REQUIRED"
        | "ARTIFACT_RANGE_INVALID"
        | "EVENT_CURSOR_INVALID" => 2,
        "WORKSPACE_NOT_INITIALIZED"
        | "CONFIG_INVALID"
        | "PROFILE_CONFIG_INVALID"
        | "PROFILE_NOT_FOUND"
        | "RUN_NOT_FOUND"
        | "THREAD_NOT_FOUND"
        | "TURN_NOT_FOUND"
        | "INTERACTION_NOT_FOUND"
        | "ARTIFACT_NOT_FOUND" => 3,
        "PROFILE_MISMATCH"
        | "COMPATIBILITY_REJECTED"
        | "DOLGORAE_PROTOCOL_MISMATCH"
        | "LEGACY_STATE_UNSUPPORTED"
        | "PROTOCOL_VERSION_UNSUPPORTED"
        | "UNSUPPORTED_SCHEMA_VERSION"
        | "SAME_HOME_MULTI_SERVER_UNSAFE" => 5,
        "TRANSPORT_FAILURE"
        | "OPERATION_TIMEOUT"
        | "PROTOCOL_FRAME_TOO_LARGE"
        | "RPC_SOCKET_UNSAFE"
        | "SERVER_SHUTDOWN"
        | "RUNTIME_PATH_INVALID"
        | "RUNTIME_PATH_COLLISION"
        | "REDACTION_FAILURE"
        | "DEDICATED_SERVER_START_FAILED"
        | "INTERNAL_ERROR" => 6,
        "TURN_FAILED" | "TURN_INTERRUPTED" => 7,
        "RUN_STATE_INVARIANT_VIOLATION"
        | "AUDIT_INTEGRITY_FAILURE"
        | "ARTIFACT_INTEGRITY_FAILURE" => 8,
        _ => 4,
    }
}

fn path_json(path: &Path) -> Value {
    use base64::Engine as _;
    use std::os::unix::ffi::OsStrExt as _;
    match path.as_os_str().to_str() {
        Some(value) => Value::String(value.to_owned()),
        None => serde_json::json!({
            "$dolgorae_path_bytes": base64::engine::general_purpose::STANDARD
                .encode(path.as_os_str().as_bytes())
        }),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FailureEnvelope {
    pub schema_version: u32,
    pub ok: bool,
    pub command: String,
    pub invocation_id: Uuid,
    pub error: MachineError,
}

impl FailureEnvelope {
    #[must_use]
    pub fn new(command: impl Into<String>, error: MachineError) -> Self {
        Self {
            schema_version: 2,
            ok: false,
            command: command.into(),
            invocation_id: new_uuid_v7(),
            error,
        }
    }
}

#[must_use]
pub fn new_uuid_v7() -> Uuid {
    Uuid::new_v7(Timestamp::now(uuid::NoContext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_envelope_is_closed_and_uuid_v7() {
        let envelope = SuccessEnvelope::new("version", serde_json::json!({"version": "0.1.0"}));
        let value = serde_json::to_value(envelope).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["ok"], true);
        assert_eq!(
            value["invocation_id"].as_str().unwrap().chars().nth(14),
            Some('7')
        );
        assert_eq!(value.as_object().unwrap().len(), 5);
    }

    #[test]
    fn current_error_exit_statuses_match_the_checked_contract() {
        for (code, expected) in registered_errors() {
            assert_eq!(exit_status_for(code), *expected, "{code}");
        }
    }

    #[test]
    fn every_exit_class_is_representable() {
        let cases = [
            ("INVALID_ARGUMENT", 2),
            ("CONTROL_MODE_REQUIRED", 2),
            ("PURPOSE_REQUIRED", 2),
            ("EXECUTION_LANE_REQUIRED", 2),
            ("ARTIFACT_RANGE_INVALID", 2),
            ("EVENT_CURSOR_INVALID", 2),
            ("RUN_NOT_FOUND", 3),
            ("ARTIFACT_NOT_FOUND", 3),
            ("RUN_STATE_CONFLICT", 4),
            ("COMPATIBILITY_REJECTED", 5),
            ("LEGACY_STATE_UNSUPPORTED", 5),
            ("SAME_HOME_MULTI_SERVER_UNSAFE", 5),
            ("TRANSPORT_FAILURE", 6),
            ("DEDICATED_SERVER_START_FAILED", 6),
            ("TURN_FAILED", 7),
            ("AUDIT_INTEGRITY_FAILURE", 8),
        ];
        for (code, expected) in cases {
            let error = MachineError {
                code: code.to_owned(),
                message: String::new(),
                retryable: false,
                details: serde_json::json!({}),
            };
            assert_eq!(error.exit_status(), expected);
        }
    }

    #[test]
    fn consumers_can_tolerate_unknown_input_fields() {
        let value = serde_json::json!({
            "schema_version": 2,
            "ok": true,
            "command": "version",
            "invocation_id": new_uuid_v7(),
            "data": {},
            "future_consumer_field": true
        });
        let parsed: SuccessEnvelope = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.command, "version");
    }
}
