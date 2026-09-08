use crate::domain::{
    Access, AggregateKind, Assurance, ControlMode, ExecutionLane, Purpose, PurposeKind,
};
use crate::jcs::sha256_hex;
use crate::machine::MachineError;
use crate::run::{
    AgentConfigurationSnapshot, AggregateBinding, AggregateMemberKind, InstructionSnapshot,
    ProfileSnapshot, RunManifest, agent_configuration_digest, runtime_profile_snapshot_digest,
};
use crate::turn::SessionSafetyPolicy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path};
use uuid::Uuid;

pub const REVIEWER_ROLE_REFERENCE: &str = "independent-reviewer-v1";
pub const REVIEWER_ROLE_SNAPSHOT: &str = concat!(
    "You are an Independent Specialist Reviewer. Inspect only the canonical working tree named ",
    "by the trusted runtime. Never modify files, Git metadata, linked worktrees, credentials, ",
    "runtime state, or another Run. Do not use approval requests, network access, nested ",
    "Specialist hiring, or an external review adapter. Return only the requested summary and ",
    "structured findings; never return hidden reasoning or raw protocol data."
);

const MAX_OBJECTIVE_BYTES: usize = 65_536;
const MAX_SUMMARY_BYTES: usize = 8_192;
const MAX_FINDINGS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum ReviewSeverity {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFinding {
    pub severity: ReviewSeverity,
    pub title: String,
    pub description: String,
    pub path: Option<String>,
    pub line_start: Option<u64>,
    pub line_end: Option<u64>,
    pub recommendation: String,
    pub confidence: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerOutput {
    pub summary: String,
    pub findings: Vec<ReviewFinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewerRuntimeRequest {
    pub runtime_profile: String,
    pub model: String,
    pub effort: String,
    pub objective: String,
    pub required_capabilities: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewerRuntimePlan {
    pub agent_configuration: AgentConfigurationSnapshot,
    pub role_snapshot_sha256: String,
    pub sandbox: &'static str,
    pub network_access: bool,
    pub approval_policy: &'static str,
    pub safety_policy: SessionSafetyPolicy,
}

impl ReviewerRuntimePlan {
    pub fn resolve(
        profile: &ProfileSnapshot,
        request: ReviewerRuntimeRequest,
    ) -> Result<Self, MachineError> {
        validate_identity(&request.runtime_profile, 128, "runtime_profile")?;
        if request.runtime_profile != profile.profile_name {
            return Err(MachineError::new(
                "REVIEW_PROFILE_UNAVAILABLE",
                "the selected Reviewer Codex Profile does not match the resolved profile",
                false,
                serde_json::json!({"profile": request.runtime_profile}),
            ));
        }
        validate_identity(&request.model, 128, "model")?;
        validate_identity(&request.effort, 64, "effort")?;
        validate_objective(&request.objective)?;
        reject_recursive_review_adapter(profile)?;

        let mut required_capabilities = request.required_capabilities;
        required_capabilities.sort();
        required_capabilities.dedup();
        if required_capabilities.len() > 64
            || required_capabilities
                .iter()
                .any(|value| validate_identity(value, 128, "required_capability").is_err())
        {
            return Err(invalid(
                "required_capabilities",
                "Reviewer capability names must be unique, bounded, and printable",
            ));
        }

        let normalized_instructions = format!(
            "{REVIEWER_ROLE_SNAPSHOT}\n\nReview objective:\n{}",
            request.objective
        );
        if normalized_instructions.len() > MAX_OBJECTIVE_BYTES {
            return Err(invalid(
                "objective",
                "Reviewer instructions exceed the checked 65536-byte bound",
            ));
        }
        let instructions = InstructionSnapshot {
            schema: "dolgorae.instructions/v1".to_owned(),
            common_prefix_version: 1,
            mode_prefix_version: 1,
            purpose_prefix_version: 1,
            normalized_byte_length: normalized_instructions.len() as u64,
            normalized_sha256: sha256_hex(normalized_instructions.as_bytes()),
        };
        let agent_configuration = AgentConfigurationSnapshot {
            schema_version: 1,
            runtime_profile: profile.profile_name.clone(),
            runtime_profile_snapshot_sha256: runtime_profile_snapshot_digest(profile)
                .map_err(internal)?,
            model: request.model,
            default_effort: request.effort,
            purpose: Purpose {
                kind: PurposeKind::Review,
                external_label: None,
            },
            required_capabilities,
            role_reference: Some(REVIEWER_ROLE_REFERENCE.to_owned()),
            normalized_instructions,
            instructions,
            execution_lane: ExecutionLane::SharedReadonly,
            required_assurance: Assurance::BestEffortPersonalAlpha,
            native_subagent_policy: "enabled".to_owned(),
        };
        Ok(Self {
            agent_configuration,
            role_snapshot_sha256: sha256_hex(REVIEWER_ROLE_SNAPSHOT.as_bytes()),
            sandbox: "read-only",
            network_access: false,
            approval_policy: "never",
            safety_policy: SessionSafetyPolicy::ReviewerReadOnly,
        })
    }

    pub fn aggregate_binding(
        &self,
        engagement_id: Uuid,
        hire_operation_id: Uuid,
    ) -> Result<AggregateBinding, MachineError> {
        if engagement_id.get_version_num() != 7 || hire_operation_id.get_version_num() != 7 {
            return Err(invalid(
                "aggregate_binding",
                "Reviewer aggregate and hire operation identities must be UUIDv7",
            ));
        }
        Ok(AggregateBinding {
            aggregate_kind: AggregateKind::ExternalSpecialistEngagement,
            aggregate_id: engagement_id,
            operation_id: hire_operation_id,
            member_kind: AggregateMemberKind::Specialist,
            policy_sha256: None,
            role_reference: Some(REVIEWER_ROLE_REFERENCE.to_owned()),
            role_snapshot_sha256: Some(self.role_snapshot_sha256.clone()),
            agent_configuration_sha256: Some(
                agent_configuration_digest(&self.agent_configuration).map_err(internal)?,
            ),
        })
    }

    pub fn validate_manifest(&self, manifest: &RunManifest) -> Result<(), MachineError> {
        let binding = manifest.aggregate_binding.as_ref().ok_or_else(|| {
            invalid(
                "aggregate_binding",
                "a Reviewer Run requires authoritative External Specialist Engagement membership",
            )
        })?;
        if manifest.control_mode != ControlMode::ManagedAgent
            || manifest.execution_lane != ExecutionLane::SharedReadonly
            || manifest.initial_access != Access::Read
            || manifest.purpose.kind != PurposeKind::Review
            || manifest.agent_configuration != self.agent_configuration
            || binding.aggregate_kind != AggregateKind::ExternalSpecialistEngagement
            || binding.member_kind != AggregateMemberKind::Specialist
            || binding.role_reference.as_deref() != Some(REVIEWER_ROLE_REFERENCE)
            || self.sandbox != "read-only"
            || self.network_access
            || self.approval_policy != "never"
            || self.safety_policy != SessionSafetyPolicy::ReviewerReadOnly
        {
            return Err(invalid(
                "reviewer_manifest",
                "the Run does not satisfy the immutable Reviewer runtime contract",
            ));
        }
        Ok(())
    }
}

pub fn validate_reviewer_output(value: Value) -> Result<ReviewerOutput, MachineError> {
    let mut output: ReviewerOutput = serde_json::from_value(value).map_err(|_| {
        MachineError::new(
            "REVIEW_OUTPUT_INVALID",
            "Reviewer output does not match the checked structured finding shape",
            false,
            serde_json::json!({"required_action":"none"}),
        )
    })?;
    if output.summary.is_empty()
        || output.summary.len() > MAX_SUMMARY_BYTES
        || output.findings.len() > MAX_FINDINGS
    {
        return Err(output_invalid("summary or finding count is out of bounds"));
    }
    for finding in &output.findings {
        validate_finding(finding)?;
    }
    output.findings.sort_by_key(|finding| finding.severity);
    Ok(output)
}

fn validate_finding(finding: &ReviewFinding) -> Result<(), MachineError> {
    for (name, value, maximum) in [
        ("title", finding.title.as_str(), 512),
        ("description", finding.description.as_str(), 8_192),
        ("recommendation", finding.recommendation.as_str(), 8_192),
    ] {
        if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
            return Err(output_invalid(&format!("finding {name} is invalid")));
        }
    }
    if !matches!(finding.confidence.as_str(), "high" | "medium" | "low") {
        return Err(output_invalid("finding confidence is invalid"));
    }
    if finding.path.as_deref().is_some_and(|path| {
        path.is_empty()
            || path.len() > 4_096
            || Path::new(path).components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
    }) {
        return Err(output_invalid("finding path is not repository-relative"));
    }
    if finding.line_start.is_some() != finding.line_end.is_some()
        || finding
            .line_start
            .zip(finding.line_end)
            .is_some_and(|(start, end)| start == 0 || end < start)
    {
        return Err(output_invalid("finding line range is invalid"));
    }
    Ok(())
}

fn reject_recursive_review_adapter(profile: &ProfileSnapshot) -> Result<(), MachineError> {
    let registered = profile
        .process_static_configuration
        .get("mcp_servers")
        .and_then(Value::as_object)
        .is_some_and(|servers| {
            servers.iter().any(|(name, configuration)| {
                name == "dolgorae_review" || recursive_review_command(configuration)
            })
        });
    if registered {
        return Err(MachineError::new(
            "REVIEW_PROFILE_UNAVAILABLE",
            "the Reviewer Codex Profile registers the recursive external review adapter",
            false,
            serde_json::json!({"required_action":"remove_dolgorae_review_from_reviewer_profile"}),
        ));
    }
    Ok(())
}

fn recursive_review_command(configuration: &Value) -> bool {
    let Some(object) = configuration.as_object() else {
        return false;
    };
    let command_is_dolgorae = object
        .get("command")
        .and_then(Value::as_str)
        .and_then(|command| std::path::Path::new(command).file_name())
        .and_then(std::ffi::OsStr::to_str)
        == Some("dolgorae");
    let arguments = object
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    command_is_dolgorae
        && (arguments
            .windows(2)
            .any(|pair| pair == ["specialist", "review"])
            || arguments.contains(&"__specialist-review-mcp"))
}

fn validate_objective(objective: &str) -> Result<(), MachineError> {
    if objective.is_empty()
        || objective.len() > MAX_OBJECTIVE_BYTES
        || objective.chars().any(|character| character.is_control())
    {
        return Err(invalid(
            "objective",
            "Reviewer objective must be nonempty, printable, and at most 65536 bytes",
        ));
    }
    Ok(())
}

fn validate_identity(value: &str, maximum: usize, field: &str) -> Result<(), MachineError> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(|character| character.is_control())
    {
        return Err(invalid(
            field,
            "value must be nonempty, bounded, and printable",
        ));
    }
    Ok(())
}

fn invalid(argument: &str, reason: &str) -> MachineError {
    MachineError::invalid_argument(argument, reason)
}

fn output_invalid(reason: &str) -> MachineError {
    MachineError::new(
        "REVIEW_OUTPUT_INVALID",
        "Reviewer output is invalid",
        false,
        serde_json::json!({"reason": reason, "required_action":"none"}),
    )
}

fn internal(reason: impl Into<String>) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "Reviewer runtime invariant failed",
        false,
        serde_json::json!({"invariant": reason.into()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::ExecutableIdentity;
    use crate::workspace::LosslessPath;
    use std::collections::{BTreeMap, BTreeSet};

    fn profile() -> ProfileSnapshot {
        let mut profile = ProfileSnapshot {
            schema_version: 1,
            profile_name: "reviewer".to_owned(),
            canonical_codex_home: "/tmp/codex-home".to_owned(),
            normalized_argv: vec!["/usr/bin/codex".to_owned()],
            launch_cwd_policy: "profile_state_directory_v1".to_owned(),
            derived_launch_cwd: "/tmp/profile".to_owned(),
            sanitized_environment: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
                ("LC_ALL".to_owned(), "en_US.UTF-8".to_owned()),
            ]),
            enabled_features: vec!["multi_agent".to_owned()],
            disabled_features: Vec::new(),
            process_static_configuration: BTreeMap::new(),
            initial_configuration_observation: BTreeMap::new(),
            executable_identity: ExecutableIdentity {
                resolved_path: LosslessPath::Utf8("/usr/bin/codex".to_owned()),
                device: 1,
                inode: 2,
                sha256: "a".repeat(64),
            },
            codex_version: "0.153.4".to_owned(),
            app_server_schema_sha256: "b".repeat(64),
            compatibility_manifest_sha256: "c".repeat(64),
            launch_contract_sha256: String::new(),
            initial_server_key: "d".repeat(64),
        };
        profile.launch_contract_sha256 = crate::run::launch_contract_digest(&profile).unwrap();
        profile
    }

    fn request() -> ReviewerRuntimeRequest {
        ReviewerRuntimeRequest {
            runtime_profile: "reviewer".to_owned(),
            model: "gpt-5".to_owned(),
            effort: "high".to_owned(),
            objective: "Review the current working tree for correctness.".to_owned(),
            required_capabilities: vec!["thread_read".to_owned()],
        }
    }

    #[test]
    fn reviewer_plan_is_immutable_independent_shared_readonly() {
        let plan = ReviewerRuntimePlan::resolve(&profile(), request()).unwrap();
        assert_eq!(
            plan.agent_configuration.execution_lane,
            ExecutionLane::SharedReadonly
        );
        assert_eq!(plan.agent_configuration.purpose.kind, PurposeKind::Review);
        assert_eq!(
            plan.agent_configuration.role_reference.as_deref(),
            Some(REVIEWER_ROLE_REFERENCE)
        );
        assert_eq!(plan.sandbox, "read-only");
        assert!(!plan.network_access);
        assert_eq!(plan.approval_policy, "never");
        assert_eq!(plan.safety_policy, SessionSafetyPolicy::ReviewerReadOnly);
        assert!(
            !plan
                .agent_configuration
                .normalized_instructions
                .contains("sentinel-secret-value")
        );
    }

    #[test]
    fn reviewer_profile_rejects_recursive_review_adapter() {
        let mut named = profile();
        named.process_static_configuration.insert(
            "mcp_servers".to_owned(),
            serde_json::json!({"dolgorae_review":{"command":"dolgorae"}}),
        );
        assert_eq!(
            ReviewerRuntimePlan::resolve(&named, request())
                .unwrap_err()
                .code,
            "REVIEW_PROFILE_UNAVAILABLE"
        );

        let mut resolved = profile();
        resolved.process_static_configuration.insert(
            "mcp_servers".to_owned(),
            serde_json::json!({"alias":{"command":"/usr/local/bin/dolgorae","args":["__specialist-review-mcp"]}}),
        );
        assert_eq!(
            ReviewerRuntimePlan::resolve(&resolved, request())
                .unwrap_err()
                .code,
            "REVIEW_PROFILE_UNAVAILABLE"
        );
    }

    #[test]
    fn reviewer_output_is_checked_and_stably_sorted() {
        let output = validate_reviewer_output(serde_json::json!({
            "summary":"two findings",
            "findings":[
                {"severity":"P3","title":"minor","description":"minor issue","path":"src/lib.rs","line_start":2,"line_end":2,"recommendation":"fix it","confidence":"low"},
                {"severity":"P0","title":"critical","description":"critical issue","path":"src/lib.rs","line_start":1,"line_end":1,"recommendation":"fix now","confidence":"high"}
            ]
        })).unwrap();
        assert_eq!(output.findings[0].severity, ReviewSeverity::P0);
        assert_eq!(output.findings[1].severity, ReviewSeverity::P3);
    }

    #[test]
    fn reviewer_output_rejects_absolute_or_parent_paths() {
        for path in ["/tmp/secret", "../outside"] {
            let error = validate_reviewer_output(serde_json::json!({
                "summary":"bad path",
                "findings":[{"severity":"P1","title":"bad","description":"bad path","path":path,"line_start":1,"line_end":1,"recommendation":"fix","confidence":"high"}]
            })).unwrap_err();
            assert_eq!(error.code, "REVIEW_OUTPUT_INVALID");
        }
    }

    #[test]
    fn aggregate_binding_is_complete_and_digest_bound() {
        let plan = ReviewerRuntimePlan::resolve(&profile(), request()).unwrap();
        let binding = plan
            .aggregate_binding(Uuid::now_v7(), Uuid::now_v7())
            .unwrap();
        assert_eq!(
            binding.aggregate_kind,
            AggregateKind::ExternalSpecialistEngagement
        );
        assert_eq!(binding.member_kind, AggregateMemberKind::Specialist);
        assert_eq!(
            binding.role_snapshot_sha256.as_deref(),
            Some(plan.role_snapshot_sha256.as_str())
        );
        assert_eq!(
            binding.agent_configuration_sha256.as_deref(),
            Some(
                agent_configuration_digest(&plan.agent_configuration)
                    .unwrap()
                    .as_str()
            )
        );
    }

    #[test]
    fn objective_is_bounded_before_configuration_exists() {
        let mut request = request();
        request.objective = "x".repeat(MAX_OBJECTIVE_BYTES + 1);
        assert_eq!(
            ReviewerRuntimePlan::resolve(&profile(), request)
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn output_severity_vocabulary_is_closed() {
        let error = validate_reviewer_output(serde_json::json!({
            "summary":"bad severity",
            "findings":[{"severity":"critical","title":"bad","description":"bad severity","path":null,"line_start":null,"line_end":null,"recommendation":"fix","confidence":"high"}]
        })).unwrap_err();
        assert_eq!(error.code, "REVIEW_OUTPUT_INVALID");
    }

    #[test]
    fn role_snapshot_contains_all_fail_closed_policies() {
        let policies = BTreeSet::from([
            "Never modify files",
            "network access",
            "nested Specialist hiring",
            "external review adapter",
            "hidden reasoning",
            "raw protocol data",
        ]);
        for policy in policies {
            assert!(
                REVIEWER_ROLE_SNAPSHOT.contains(policy),
                "missing policy {policy}"
            );
        }
    }
}
