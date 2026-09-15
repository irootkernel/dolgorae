use crate::domain::{
    Access, AggregateKind, Assurance, ControlMode, ExecutionLane, Purpose, PurposeKind,
};
use crate::jcs::sha256_hex;
use crate::machine::MachineError;
use crate::run::{
    AgentConfigurationSnapshot, AggregateBinding, AggregateMemberKind, InstructionSnapshot,
    ProfileSnapshot, RunManifest, agent_configuration_digest, runtime_profile_snapshot_digest,
};
use crate::task_request::SpecialistTaskRequest;
use crate::turn::SessionSafetyPolicy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path};
use uuid::Uuid;

pub const REVIEWER_ROLE_REFERENCE: &str = "independent-reviewer-v1";
pub const REVIEWER_ROLE_SNAPSHOT: &str = concat!(
    "You are an Independent Specialist Reviewer. Inspect only the candidate rooted in the current ",
    "directory and named by the trusted runtime. Never modify files, Git metadata, linked ",
    "worktrees, credentials, ",
    "runtime state, or another Run. Do not use approval requests, network access, nested ",
    "Specialist hiring, or an external review adapter. Return only the checked structured report ",
    "requested by the accepted task; never return hidden reasoning or raw protocol data."
);

const MAX_SUMMARY_BYTES: usize = 8_192;
const MAX_FINDINGS: usize = 64;
const MAX_REVIEW_OUTPUT_BYTES: usize = 1_048_576;

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAssessmentEvidence {
    pub basis: String,
    pub description: String,
    pub path: Option<String>,
    pub line_start: Option<u64>,
    pub line_end: Option<u64>,
    pub context_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionAssessment {
    pub criterion_id: String,
    pub status: String,
    pub explanation: String,
    pub evidence: Vec<ReviewAssessmentEvidence>,
    pub remaining_gap: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerOutputV3 {
    pub summary: String,
    pub findings: Vec<ReviewFinding>,
    pub criterion_assessments: Vec<CriterionAssessment>,
    pub evidence_limits: Vec<String>,
    pub overall_assessment: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewerRuntimeRequest {
    pub runtime_profile: String,
    pub model: String,
    pub effort: String,
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

        let normalized_instructions = REVIEWER_ROLE_SNAPSHOT.to_owned();
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
        output_invalid("output does not match the checked structured finding shape")
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

pub(crate) fn reviewer_turn_output_value(turn: &Value) -> Result<Value, MachineError> {
    let status = turn.get("status").and_then(Value::as_str);
    if matches!(status, Some("running" | "accepted")) {
        return Err(MachineError::new(
            "REVIEW_TIMEOUT",
            "Reviewer did not finish before the bounded deadline",
            false,
            serde_json::json!({"required_action":"none"}),
        ));
    }
    if status != Some("completed") {
        return Err(MachineError::new(
            "REVIEW_TASK_FAILED",
            "Reviewer Turn was not completed",
            false,
            serde_json::json!({"required_action":"none"}),
        ));
    }
    let text = turn
        .pointer("/final_response/text")
        .and_then(Value::as_str)
        .ok_or_else(|| output_invalid("Reviewer returned no inline JSON"))?;
    serde_json::from_str(text).map_err(|_| output_invalid("Reviewer output is not JSON"))
}

pub fn validate_reviewer_output_v3(
    value: Value,
    task: &SpecialistTaskRequest,
) -> Result<ReviewerOutputV3, MachineError> {
    task.validate_review()?;
    let mut output: ReviewerOutputV3 = serde_json::from_value(value)
        .map_err(|_| output_invalid("output does not match structured_review_v3"))?;
    if output.summary.is_empty()
        || output.summary.len() > MAX_SUMMARY_BYTES
        || output.findings.len() > MAX_FINDINGS
        || output.criterion_assessments.len() != task.criteria.len()
        || output.evidence_limits.len() > 64
        || serde_json::to_vec(&output)
            .map_err(|error| internal(error.to_string()))?
            .len()
            > MAX_REVIEW_OUTPUT_BYTES
    {
        return Err(output_invalid(
            "v3 report or collection count is out of bounds",
        ));
    }
    for finding in &output.findings {
        validate_finding(finding)?;
    }
    output.findings.sort_by_key(|finding| finding.severity);
    for (criterion, assessment) in task.criteria.iter().zip(&output.criterion_assessments) {
        if assessment.criterion_id != criterion.id {
            return Err(output_invalid(
                "criterion assessments must cover the accepted criteria once in input order",
            ));
        }
        if !matches!(
            assessment.status.as_str(),
            "met" | "unmet" | "unverified" | "not_applicable"
        ) {
            return Err(output_invalid("criterion status is invalid"));
        }
        output_text(&assessment.explanation, 8_192, "criterion explanation")?;
        if assessment.evidence.is_empty() || assessment.evidence.len() > 64 {
            return Err(output_invalid("criterion evidence count is invalid"));
        }
        if let Some(gap) = &assessment.remaining_gap {
            output_text(gap, 8_192, "remaining gap")?;
        }
        let source_context_ids = criterion
            .source_context_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        for evidence in &assessment.evidence {
            validate_assessment_evidence(evidence, &source_context_ids)?;
        }
    }
    for limit in &output.evidence_limits {
        output_text(limit, 4_096, "evidence limit")?;
    }
    if !matches!(
        output.overall_assessment.as_str(),
        "requirements_met" | "requirements_not_met" | "insufficient_evidence"
    ) {
        return Err(output_invalid("overall assessment is invalid"));
    }
    let has_unmet = output
        .criterion_assessments
        .iter()
        .any(|assessment| assessment.status == "unmet");
    let has_unverified = output
        .criterion_assessments
        .iter()
        .any(|assessment| assessment.status == "unverified");
    if (has_unmet && output.overall_assessment != "requirements_not_met")
        || (!has_unmet && has_unverified && output.overall_assessment != "insufficient_evidence")
        || (!has_unmet && !has_unverified && output.overall_assessment != "requirements_met")
    {
        return Err(output_invalid(
            "overall assessment conflicts with criterion statuses",
        ));
    }
    Ok(output)
}

fn validate_assessment_evidence(
    evidence: &ReviewAssessmentEvidence,
    source_context_ids: &std::collections::BTreeSet<&str>,
) -> Result<(), MachineError> {
    if !matches!(
        evidence.basis.as_str(),
        "candidate" | "context" | "caller_reported" | "unavailable"
    ) {
        return Err(output_invalid("assessment evidence basis is invalid"));
    }
    output_text(
        &evidence.description,
        8_192,
        "assessment evidence description",
    )?;
    if evidence.path.as_deref().is_some_and(invalid_relative_path) {
        return Err(output_invalid("assessment evidence path is invalid"));
    }
    if evidence.line_start.is_some() != evidence.line_end.is_some()
        || evidence
            .line_start
            .zip(evidence.line_end)
            .is_some_and(|(start, end)| start == 0 || end < start)
    {
        return Err(output_invalid("assessment evidence line range is invalid"));
    }
    if evidence.line_start.is_some() && evidence.path.is_none() {
        return Err(output_invalid(
            "assessment evidence line range requires a candidate path",
        ));
    }
    match evidence.basis.as_str() {
        "context"
            if !evidence
                .context_id
                .as_deref()
                .is_some_and(|id| source_context_ids.contains(id)) =>
        {
            Err(output_invalid(
                "assessment references context not declared for its criterion",
            ))
        }
        "context" if evidence.path.is_some() || evidence.line_start.is_some() => Err(
            output_invalid("context evidence cannot carry a candidate source location"),
        ),
        "context" => Ok(()),
        _ if evidence.context_id.is_some() => Err(output_invalid(
            "only context evidence may carry a context ID",
        )),
        "caller_reported" | "unavailable"
            if evidence.path.is_some() || evidence.line_start.is_some() =>
        {
            Err(output_invalid(
                "non-candidate evidence cannot carry a candidate source location",
            ))
        }
        _ => Ok(()),
    }
}

fn output_text(value: &str, maximum: usize, label: &str) -> Result<(), MachineError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        Err(output_invalid(&format!("{label} is invalid")))
    } else {
        Ok(())
    }
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
    if finding.path.as_deref().is_some_and(invalid_relative_path) {
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

fn invalid_relative_path(path: &str) -> bool {
    path.is_empty()
        || path.len() > 4_096
        || Path::new(path).components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
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

pub(crate) fn output_invalid(reason: &str) -> MachineError {
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
    use crate::task_request::{
        STRUCTURED_REVIEW_OUTPUT, SpecialistTaskRequest, TaskContext, TaskCriterion,
    };
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
    fn v3_output_covers_each_accepted_criterion_in_order() {
        let task = SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "Assess completion.".to_owned(),
            contexts: vec![TaskContext {
                id: "requirements".to_owned(),
                content: "C-1 is authoritative.".to_owned(),
                provenance: "approved specification".to_owned(),
            }],
            criteria: vec![TaskCriterion {
                id: "C-1".to_owned(),
                statement: "The behavior exists.".to_owned(),
                source_context_ids: vec!["requirements".to_owned()],
            }],
            expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
        };
        let output = validate_reviewer_output_v3(
            serde_json::json!({
                "summary":"criterion is not yet satisfied",
                "findings":[],
                "criterion_assessments":[{
                    "criterion_id":"C-1",
                    "status":"unmet",
                    "explanation":"Production wiring is absent.",
                    "evidence":[{
                        "basis":"candidate",
                        "description":"No call site exists in the captured candidate.",
                        "path":null,
                        "line_start":null,
                        "line_end":null,
                        "context_id":null
                    }],
                    "remaining_gap":"Add production wiring."
                }],
                "evidence_limits":[],
                "overall_assessment":"requirements_not_met"
            }),
            &task,
        )
        .unwrap();
        assert_eq!(output.criterion_assessments[0].criterion_id, "C-1");

        let error = validate_reviewer_output_v3(
            serde_json::json!({
                "summary":"missing assessment",
                "findings":[],
                "criterion_assessments":[],
                "evidence_limits":[],
                "overall_assessment":"requirements_met"
            }),
            &task,
        )
        .unwrap_err();
        assert_eq!(error.code, "REVIEW_OUTPUT_INVALID");
    }

    #[test]
    fn v3_output_preserves_all_status_and_evidence_variants() {
        let task = SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "Assess every criterion.".to_owned(),
            contexts: vec![TaskContext {
                id: "requirements".to_owned(),
                content: "authoritative criteria".to_owned(),
                provenance: "approved specification".to_owned(),
            }],
            criteria: ["met", "unmet", "unverified", "not-applicable"]
                .into_iter()
                .map(|id| TaskCriterion {
                    id: id.to_owned(),
                    statement: format!("Assess {id}."),
                    source_context_ids: vec!["requirements".to_owned()],
                })
                .collect(),
            expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
        };
        let report = serde_json::json!({
            "summary":"mixed assessment",
            "findings":[],
            "criterion_assessments":[
                {
                    "criterion_id":"met",
                    "status":"met",
                    "explanation":"candidate line proves it",
                    "evidence":[{"basis":"candidate","description":"implementation","path":"src/lib.rs","line_start":1,"line_end":1,"context_id":null}],
                    "remaining_gap":null
                },
                {
                    "criterion_id":"unmet",
                    "status":"unmet",
                    "explanation":"the accepted requirement is not implemented",
                    "evidence":[{"basis":"context","description":"approved requirement","path":null,"line_start":null,"line_end":null,"context_id":"requirements"}],
                    "remaining_gap":"implement the requirement"
                },
                {
                    "criterion_id":"unverified",
                    "status":"unverified",
                    "explanation":"runtime evidence is unavailable",
                    "evidence":[{"basis":"unavailable","description":"live check not authorized","path":null,"line_start":null,"line_end":null,"context_id":null}],
                    "remaining_gap":"run the authorized live check"
                },
                {
                    "criterion_id":"not-applicable",
                    "status":"not_applicable",
                    "explanation":"the caller excluded this platform",
                    "evidence":[{"basis":"caller_reported","description":"approved scope exclusion","path":null,"line_start":null,"line_end":null,"context_id":null}],
                    "remaining_gap":null
                }
            ],
            "evidence_limits":["no live provider check"],
            "overall_assessment":"requirements_not_met"
        });
        let checked = validate_reviewer_output_v3(report.clone(), &task).unwrap();
        assert_eq!(checked.criterion_assessments.len(), 4);

        let mut reordered = report;
        reordered["criterion_assessments"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        assert_eq!(
            validate_reviewer_output_v3(reordered, &task)
                .unwrap_err()
                .code,
            "REVIEW_OUTPUT_INVALID"
        );
    }

    #[test]
    fn v3_output_rejects_undeclared_context_and_oversized_text() {
        let task = SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "Assess completion.".to_owned(),
            contexts: vec![TaskContext {
                id: "other".to_owned(),
                content: "context for another criterion".to_owned(),
                provenance: "approved specification".to_owned(),
            }],
            criteria: vec![TaskCriterion {
                id: "C-1".to_owned(),
                statement: "The behavior exists.".to_owned(),
                source_context_ids: vec![],
            }],
            expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
        };
        let report = serde_json::json!({
            "summary":"reviewed",
            "findings":[],
            "criterion_assessments":[{
                "criterion_id":"C-1",
                "status":"unverified",
                "explanation":"context is unavailable",
                "evidence":[{"basis":"context","description":"undeclared context","path":null,"line_start":null,"line_end":null,"context_id":"other"}],
                "remaining_gap":"supply context"
            }],
            "evidence_limits":[],
            "overall_assessment":"insufficient_evidence"
        });
        assert_eq!(
            validate_reviewer_output_v3(report, &task).unwrap_err().code,
            "REVIEW_OUTPUT_INVALID"
        );

        let oversized = serde_json::json!({
            "summary":"x".repeat(MAX_SUMMARY_BYTES + 1),
            "findings":[],
            "criterion_assessments":[{
                "criterion_id":"C-1",
                "status":"met",
                "explanation":"checked",
                "evidence":[{"basis":"candidate","description":"candidate","path":null,"line_start":null,"line_end":null,"context_id":null}],
                "remaining_gap":null
            }],
            "evidence_limits":[],
            "overall_assessment":"requirements_met"
        });
        assert_eq!(
            validate_reviewer_output_v3(oversized, &task)
                .unwrap_err()
                .code,
            "REVIEW_OUTPUT_INVALID"
        );
    }

    #[test]
    fn v3_output_rejects_criterion_identity_evidence_and_overall_mismatches() {
        let task = SpecialistTaskRequest {
            purpose: "completion".to_owned(),
            brief: "Assess both criteria.".to_owned(),
            contexts: vec![TaskContext {
                id: "requirements".to_owned(),
                content: "Accepted requirements.".to_owned(),
                provenance: "approved specification".to_owned(),
            }],
            criteria: ["C-1", "C-2"]
                .into_iter()
                .map(|id| TaskCriterion {
                    id: id.to_owned(),
                    statement: format!("Assess {id}."),
                    source_context_ids: vec!["requirements".to_owned()],
                })
                .collect(),
            expected_output: STRUCTURED_REVIEW_OUTPUT.to_owned(),
        };
        let report = serde_json::json!({
            "summary":"both criteria are met",
            "findings":[],
            "criterion_assessments":[
                {
                    "criterion_id":"C-1",
                    "status":"met",
                    "explanation":"candidate evidence",
                    "evidence":[{"basis":"candidate","description":"implementation","path":"src/lib.rs","line_start":1,"line_end":1,"context_id":null}],
                    "remaining_gap":null
                },
                {
                    "criterion_id":"C-2",
                    "status":"met",
                    "explanation":"accepted context",
                    "evidence":[{"basis":"context","description":"requirement","path":null,"line_start":null,"line_end":null,"context_id":"requirements"}],
                    "remaining_gap":null
                }
            ],
            "evidence_limits":[],
            "overall_assessment":"requirements_met"
        });

        for criterion_id in ["C-1", "unknown"] {
            let mut invalid = report.clone();
            invalid["criterion_assessments"][1]["criterion_id"] = criterion_id.into();
            assert_eq!(
                validate_reviewer_output_v3(invalid, &task)
                    .unwrap_err()
                    .code,
                "REVIEW_OUTPUT_INVALID"
            );
        }

        let mut malformed_evidence = report.clone();
        malformed_evidence["criterion_assessments"][1]["evidence"][0]["context_id"] = Value::Null;
        assert_eq!(
            validate_reviewer_output_v3(malformed_evidence, &task)
                .unwrap_err()
                .code,
            "REVIEW_OUTPUT_INVALID"
        );

        let mut inconsistent_overall = report;
        inconsistent_overall["criterion_assessments"][0]["status"] = "unverified".into();
        assert_eq!(
            validate_reviewer_output_v3(inconsistent_overall, &task)
                .unwrap_err()
                .code,
            "REVIEW_OUTPUT_INVALID"
        );
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
    fn task_text_is_absent_from_stable_reviewer_configuration() {
        let first = ReviewerRuntimePlan::resolve(&profile(), request()).unwrap();
        let second = ReviewerRuntimePlan::resolve(&profile(), request()).unwrap();
        assert_eq!(first.agent_configuration, second.agent_configuration);
        assert_eq!(
            first.agent_configuration.normalized_instructions,
            REVIEWER_ROLE_SNAPSHOT
        );
        assert!(
            !first
                .agent_configuration
                .normalized_instructions
                .contains("Review objective")
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
