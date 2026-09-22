use crate::protocol::PUBLIC_V1_DESCRIPTOR_SHA256;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    pub dolgorae_version: String,
    pub machine_protocol_version: u32,
    pub event_protocol_version: u32,
    pub rpc_protocol_version: u32,
    pub timeline_protocol_version: u32,
    pub event_projection_version: u32,
    pub grpc_error_detail_version: u32,
    pub minimum_rpc_client_version: u32,
    pub maximum_rpc_client_version: u32,
    pub rpc_descriptor_sha256: String,
    pub grpc_methods: Vec<String>,
    pub controller_carrier_root: String,
    pub controller_credential: ControllerCredentialCapability,
    pub artifact_bounds: ArtifactBounds,
    pub supported_transports: Vec<String>,
    pub app_server_transport: String,
    pub profile_launch_mode: String,
    pub projection_profiles: Vec<String>,
    pub control_modes: Vec<String>,
    pub execution_lanes: Vec<String>,
    pub assurance: AssuranceCapability,
    pub lane_capabilities: BTreeMap<String, LaneCapability>,
    pub independent_run_concurrency: IndependentRunConcurrency,
    pub features: RuntimeFeatures,
    pub interactions: InteractionCapabilities,
    pub access_policy_transition: String,
    pub background_execution_control: BackgroundExecutionControl,
    pub native_subagents: NativeSubagentCapabilities,
    pub native_subagents_policy: String,
    pub profile: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControllerCredentialCapability {
    pub schema_id: String,
    pub schema_version: u32,
    pub schema_sha256: String,
    pub accepted_kinds: Vec<String>,
    pub capability_byte_length: u32,
    pub capability_encoding: String,
    pub parent_directory_mode: String,
    pub file_mode: String,
    pub same_uid: bool,
    pub regular_file: bool,
    pub symlinks: String,
    pub create_exclusive: bool,
    pub maximum_file_bytes: u32,
    pub client_descendant_pattern: String,
    pub normalized_principal: String,
    pub initial_generation: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactBounds {
    pub maximum_artifact_bytes: u32,
    pub maximum_chunk_bytes: u32,
    pub maximum_inline_response_bytes: u32,
    pub digest: String,
    pub exact_byte_length: bool,
    pub visibility_classes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssuranceCapability {
    pub supported: Vec<String>,
    pub maximum_achievable: String,
    pub selection_time: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaneCapability {
    pub command_execution: String,
    pub background_control: String,
    pub per_run_process_cleanup: String,
    pub maximum_assurance: String,
    pub writer_support: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical_start: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndependentRunConcurrency {
    pub basic_same_home_coexistence: String,
    pub storage_and_long_duration: String,
    pub resource_warning_live_dedicated: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InteractionCapabilities {
    pub command_execution_approval: String,
    pub file_change_approval: String,
    pub permission_request: String,
    pub user_input: String,
    pub mcp_elicitation: String,
    pub connector_approval: String,
    pub maximum_response_bytes: u32,
    pub maximum_safe_payload_bytes: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackgroundExecutionControl {
    pub support: String,
    pub mechanism: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeSubagentCapabilities {
    pub lifecycle_observation: String,
    pub disable_enforcement: String,
    pub quiescence_tracking: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeFeatures {
    pub persistent_runs: bool,
    pub run_fork: bool,
    pub reader_writer_access: bool,
    pub threadless_acquire_write: bool,
    pub first_write_via_submit_turn: bool,
    pub write_continuation: bool,
    pub controller_timeline: bool,
    pub writer_handoff: bool,
    pub durable_writer_authority: bool,
    pub sticky_dedicated_lanes: bool,
    pub control_modes: bool,
    pub brokered_independent_subagent_runs: bool,
    pub assurance_negotiation: bool,
    pub profile_server_migration: bool,
    pub profile_membership_repair: bool,
    pub event_replay: bool,
    pub artifact_retrieval: bool,
    pub profile_diagnostics: bool,
    pub controller_binding: bool,
    pub worker_controller_revalidation: bool,
    pub operator_capability: bool,
    pub operator_controller_reset: bool,
    pub safe_client_projection: bool,
    pub public_local_socket: bool,
    pub workspace_event_stream: bool,
}

impl RuntimeFeatures {
    #[must_use]
    pub const fn task_001() -> Self {
        Self {
            persistent_runs: false,
            run_fork: false,
            reader_writer_access: false,
            threadless_acquire_write: false,
            first_write_via_submit_turn: false,
            write_continuation: false,
            controller_timeline: false,
            writer_handoff: false,
            durable_writer_authority: false,
            sticky_dedicated_lanes: false,
            control_modes: false,
            brokered_independent_subagent_runs: false,
            assurance_negotiation: false,
            profile_server_migration: false,
            profile_membership_repair: false,
            event_replay: false,
            artifact_retrieval: false,
            profile_diagnostics: false,
            controller_binding: false,
            worker_controller_revalidation: false,
            operator_capability: false,
            operator_controller_reset: false,
            safe_client_projection: false,
            public_local_socket: false,
            workspace_event_stream: false,
        }
    }

    #[must_use]
    pub const fn task_005() -> Self {
        let mut features = Self::task_001();
        features.profile_server_migration = true;
        features.profile_membership_repair = true;
        features.profile_diagnostics = true;
        features
    }

    #[must_use]
    pub const fn task_007() -> Self {
        let mut features = Self::task_005();
        features.controller_binding = true;
        // Every mutating run control request (send, submit, respond,
        // interrupt, close) now carries the caller's already-open Controller
        // descriptor to the worker over SCM_RIGHTS, and the worker rereads and
        // revalidates it against the reset-journal-reconciled current binding
        // while holding the Run mutation lock, immediately before effects.
        features.worker_controller_revalidation = true;
        features.operator_capability = true;
        features.operator_controller_reset = true;
        // TASK-007 leaves safe_client_projection false. TASK-023 enables it
        // only after the public gateway supplies the complete observer-safe
        // Run, event, timeline, and pending-interaction projections required
        // by ADR-016 without extending the frozen control-v1 response.
        features
    }

    #[must_use]
    pub const fn task_023() -> Self {
        let mut features = Self::task_007();
        features.persistent_runs = true;
        features.first_write_via_submit_turn = true;
        features.durable_writer_authority = true;
        features.sticky_dedicated_lanes = true;
        features.control_modes = true;
        features.event_replay = true;
        features.controller_timeline = true;
        features.artifact_retrieval = true;
        features.safe_client_projection = true;
        features.public_local_socket = true;
        features
    }
}

#[must_use]
pub fn capabilities() -> RuntimeCapabilities {
    serde_json::from_value(json!({
        "dolgorae_version": env!("CARGO_PKG_VERSION"),
        "machine_protocol_version": 1,
        "event_protocol_version": 1,
        "rpc_protocol_version": 1,
        "timeline_protocol_version": 1,
        "event_projection_version": 1,
        "grpc_error_detail_version": 1,
        "minimum_rpc_client_version": 1,
        "maximum_rpc_client_version": 1,
        "rpc_descriptor_sha256": PUBLIC_V1_DESCRIPTOR_SHA256,
        "grpc_methods": [
            "ArtifactService.GetArtifact",
            "ArtifactService.ReadArtifactChunk",
            "ControllerService.VerifyController",
            "InteractionService.GetControllerInteraction",
            "InteractionService.ListPendingInteractions",
            "InteractionService.ResolveInteraction",
            "ObservationService.ListRunTimelineItems",
            "ObservationService.WatchRunEvents",
            "OrchestrationService.GetOrchestratedSession",
            "OrchestrationService.ListOrchestratedSessionResults",
            "RunService.CloseRun",
            "RunService.GetRun",
            "RunService.InterruptTurn",
            "RunService.ListRuns",
            "RunService.PauseRun",
            "RunService.ReconcileRun",
            "RunService.RecoverRun",
            "RunService.ResumeRun",
            "RunService.StartRun",
            "RunService.SubmitTurn",
            "RuntimeService.GetCapabilities",
            "RuntimeService.GetProfile",
            "RuntimeService.InspectWorkspace",
            "RuntimeService.ListProfiles",
            "WriterService.AcquireWriter",
            "WriterService.GetWorkspaceWriterStatus",
            "WriterService.ReleaseWriter"
],
        "controller_carrier_root": "home/.dolgorae/controller-carriers",
        "controller_credential": {
            "schema_id": "https://dolgorae.local/schema/controller-credential/v1",
            "schema_version": 1,
            "schema_sha256": "6e888023fa6f12964afbd2867832944307dc626cad3fe6c6bc517768ab98b84f",
            "accepted_kinds": ["human_cli", "interactive_client", "workflow_orchestrator", "automation", "other"],
            "capability_byte_length": 32,
            "capability_encoding": "base64url_no_padding",
            "parent_directory_mode": "0700",
            "file_mode": "0600",
            "same_uid": true,
            "regular_file": true,
            "symlinks": "forbidden",
            "create_exclusive": true,
            "maximum_file_bytes": 4096,
            "client_descendant_pattern": "<client>/<installation-id>/",
            "normalized_principal": "kind+subject_id_else_kind+instance_id",
            "initial_generation": 1
        },
        "artifact_bounds": {
            "maximum_artifact_bytes": 33554432,
            "maximum_chunk_bytes": 1048576,
            "maximum_inline_response_bytes": 1048576,
            "digest": "sha256",
            "exact_byte_length": true,
            "visibility_classes": ["observer", "controller_only"]
        },
        "supported_transports": ["machine_cli", "local_grpc"],
        "app_server_transport": "direct_websocket_unix",
        "profile_launch_mode": "dolgorae_owned_direct_executable",
        "projection_profiles": ["minimal", "operational"],
        "control_modes": ["direct_interactive", "managed_agent"],
        "execution_lanes": ["shared_readonly", "dedicated"],
        "assurance": {
            "supported": ["best_effort_personal_alpha"],
            "maximum_achievable": "best_effort_personal_alpha",
            "selection_time": "before_run_allocation"
        },
        "lane_capabilities": {
            "shared_readonly": {
                "command_execution": "bounded_best_effort",
                "background_control": "profile_aggregate_only",
                "per_run_process_cleanup": "unavailable",
                "maximum_assurance": "best_effort_personal_alpha",
                "writer_support": false,
                "codex_mode": "plan"
            },
            "dedicated": {
                "command_execution": "supported",
                "background_control": "best_effort_personal_alpha",
                "per_run_process_cleanup": "bounded_process_census",
                "maximum_assurance": "best_effort_personal_alpha",
                "writer_support": true,
                "physical_start": "lazy_first_input"
            }
        },
        "independent_run_concurrency": {
            "basic_same_home_coexistence": "unverified",
            "storage_and_long_duration": "unverified",
            "resource_warning_live_dedicated": 6
        },
        "features": RuntimeFeatures::task_023(),
        "interactions": {
            "command_execution_approval": "supported",
            "file_change_approval": "supported",
            "permission_request": "recognized_unsupported",
            "user_input": "supported",
            "mcp_elicitation": "recognized_unsupported",
            "connector_approval": "recognized_unsupported",
            "maximum_response_bytes": 1048576,
            "maximum_safe_payload_bytes": 8388608
        },
        "access_policy_transition": "unverified",
        "background_execution_control": {
            "support": "unverified",
            "mechanism": "dedicated_lane_process_census"
        },
        "native_subagents": {
            "lifecycle_observation": "unverified",
            "disable_enforcement": "unavailable",
            "quiescence_tracking": "unverified",
            "reason": "TASK-005 validates the enabled-only launch policy, while complete native child observation remains unverified until a run exercises it."
        },
        "native_subagents_policy": "enabled",
        "profile": null
    }))
    .expect("checked typed TASK-001 capability document")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn task_055_advertises_the_implemented_public_surface() {
        let capabilities = serde_json::to_value(capabilities()).unwrap();
        assert_eq!(capabilities["features"]["persistent_runs"], true);
        assert_eq!(
            capabilities["features"]["brokered_independent_subagent_runs"],
            false
        );
        assert_eq!(capabilities["features"]["profile_server_migration"], true);
        assert_eq!(capabilities["features"]["profile_membership_repair"], true);
        assert_eq!(capabilities["features"]["controller_binding"], true);
        assert_eq!(
            capabilities["features"]["worker_controller_revalidation"],
            true
        );
        assert_eq!(capabilities["features"]["operator_capability"], true);
        assert_eq!(capabilities["features"]["operator_controller_reset"], true);
        assert_eq!(capabilities["features"]["safe_client_projection"], true);
        assert_eq!(capabilities["features"]["public_local_socket"], true);
        assert_eq!(capabilities["features"]["controller_timeline"], true);
        assert_eq!(
            capabilities["supported_transports"],
            json!(["machine_cli", "local_grpc"])
        );
        assert_eq!(capabilities["grpc_methods"].as_array().unwrap().len(), 27);
        assert!(
            capabilities["grpc_methods"]
                .as_array()
                .unwrap()
                .contains(&json!("ObservationService.ListRunTimelineItems"))
        );
        assert!(
            capabilities["grpc_methods"]
                .as_array()
                .unwrap()
                .contains(&json!("OrchestrationService.GetOrchestratedSession"))
        );
        assert!(
            capabilities["grpc_methods"]
                .as_array()
                .unwrap()
                .contains(&json!(
                    "OrchestrationService.ListOrchestratedSessionResults"
                ))
        );
    }

    #[test]
    fn advertised_controller_credential_digest_matches_distributed_bytes() {
        let bytes =
            include_bytes!("../docs/protocol/dolgorae-controller-credential-v1.schema.json");
        let expected = format!("{:x}", Sha256::digest(bytes));
        assert_eq!(capabilities().controller_credential.schema_sha256, expected);
    }
}
