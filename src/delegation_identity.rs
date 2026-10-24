//! Exact captured instruction identities for successor delegation.
//!
//! This module owns byte validation and H1 identity computation, not source
//! discovery, allocation, authorization, or execution. Old captures keep their
//! existing readers; the successor never reloads their former source files.

use crate::domain::MAX_JCS_SAFE_INTEGER;
use crate::instructions::{
    CORE_PREFIX, CORE_PREFIX_VERSION, MAX_SOURCE_BYTES, RoleCapture, SourceCapture,
};
use crate::jcs::{canonicalize, parse, sha256_hex};
use crate::machine::MachineError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const ROLE_SCHEMA: &str = "dolgorae.role-snapshot/v1";
pub const INSTRUCTION_SCHEMA: &str = "dolgorae.instruction-identity/v1";
pub const CONFIGURATION_SCHEMA: &str = "dolgorae.admitted-configuration/v1";
pub const ACCEPTED_TASK_SCHEMA: &str = "dolgorae.accepted-task/v1";
const COMPOSITION_VERSION: &str = "core-common-role-dependencies-guidance-lf2/v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BytesRef {
    pub byte_length: u64,
    pub sha256: String,
}

impl BytesRef {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            byte_length: bytes.len() as u64,
            sha256: format!("sha256:{}", sha256_hex(bytes)),
        }
    }

    fn validate(&self, minimum: u64, maximum: u64) -> Result<(), MachineError> {
        if !(minimum..=maximum).contains(&self.byte_length) || !prefixed_digest(&self.sha256) {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedSource {
    pub name: String,
    pub content: BytesRef,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedSourceCapture {
    pub name: String,
    pub source: SourceCapture,
}

impl NamedSourceCapture {
    pub fn new(name: String, body: &str) -> Result<Self, MachineError> {
        validate_alias(&name)?;
        Ok(Self {
            name,
            source: SourceCapture::from_body(body)?,
        })
    }

    fn reference(&self) -> NamedSource {
        NamedSource {
            name: self.name.clone(),
            content: BytesRef::from_bytes(self.source.body.as_bytes()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSnapshot {
    pub schema: String,
    pub role_body: BytesRef,
    pub dependencies: Vec<NamedSource>,
    pub selected_guidance: Vec<NamedSource>,
}

impl RoleSnapshot {
    pub fn digest(&self) -> Result<String, MachineError> {
        if self.schema != ROLE_SCHEMA {
            return Err(invalid());
        }
        self.role_body.validate(1, MAX_SOURCE_BYTES as u64)?;
        validate_named_references(&self.dependencies, true)?;
        validate_named_references(&self.selected_guidance, false)?;
        // The 65,536-byte aggregate belongs to instruction-identity/v1, which
        // spans common, Role, dependencies and guidance. This object only bounds
        // its own Role body and named sources.
        domain_digest(ROLE_SCHEMA, self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionIdentity {
    pub schema: String,
    pub core_version: u64,
    pub core: BytesRef,
    pub common: BytesRef,
    pub role_snapshot_digest: String,
    pub dependency_order: Vec<String>,
    pub guidance_order: Vec<String>,
    pub composition_version: String,
    pub composition: BytesRef,
}

impl InstructionIdentity {
    pub fn digest(&self) -> Result<String, MachineError> {
        if self.schema != INSTRUCTION_SCHEMA
            || self.core_version > MAX_JCS_SAFE_INTEGER
            || self.composition_version != COMPOSITION_VERSION
            || !prefixed_digest(&self.role_snapshot_digest)
        {
            return Err(invalid());
        }
        self.core.validate(1, MAX_JCS_SAFE_INTEGER)?;
        self.common.validate(1, MAX_SOURCE_BYTES as u64)?;
        self.composition.validate(1, MAX_JCS_SAFE_INTEGER)?;
        validate_aliases(self.dependency_order.iter().map(String::as_str), true)?;
        validate_aliases(self.guidance_order.iter().map(String::as_str), false)?;
        domain_digest(INSTRUCTION_SCHEMA, self)
    }
}

fn validate_named_references(sources: &[NamedSource], sorted: bool) -> Result<(), MachineError> {
    validate_aliases(sources.iter().map(|source| source.name.as_str()), sorted)?;
    for source in sources {
        source.content.validate(1, MAX_SOURCE_BYTES as u64)?;
    }
    Ok(())
}

fn validate_aliases<'a>(
    names: impl Iterator<Item = &'a str>,
    sorted: bool,
) -> Result<(), MachineError> {
    let mut seen = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for name in names {
        validate_alias(name)?;
        if !seen.insert(name)
            || seen.len() > 64
            || (sorted && previous.is_some_and(|value| value.as_bytes() >= name.as_bytes()))
        {
            return Err(invalid());
        }
        previous = Some(name);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmittedConfiguration {
    pub schema: String,
    pub profile_binding_schema: String,
    pub profile_binding_digest: String,
    pub model: String,
    pub effort: String,
    pub purpose_kind: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub purpose_label: Option<String>,
    pub required_capabilities: Vec<String>,
    pub execution_lane: String,
    pub requested_access: String,
    pub required_assurance: String,
    pub native_subagent_policy: String,
    pub instruction_identity_digest: String,
}

impl AdmittedConfiguration {
    pub fn digest(&self) -> Result<String, MachineError> {
        if self.schema != CONFIGURATION_SCHEMA
            || !bounded(&self.profile_binding_schema, 256)
            || !prefixed_digest(&self.profile_binding_digest)
            || !prefixed_digest(&self.instruction_identity_digest)
            || !bounded(&self.model, 256)
            || !bounded(&self.effort, 128)
            || !matches!(
                self.purpose_kind.as_str(),
                "interactive"
                    | "planning"
                    | "implementation"
                    | "review"
                    | "research"
                    | "discussion"
                    | "workflow_stage"
                    | "other"
            )
            || self
                .purpose_label
                .as_ref()
                .is_some_and(|label| !bounded(label, 128))
            || !matches!(
                self.execution_lane.as_str(),
                "dedicated" | "shared_readonly"
            )
            || !matches!(
                self.requested_access.as_str(),
                "read_only" | "canonical_workspace_write"
            )
            || !matches!(
                self.required_assurance.as_str(),
                "best_effort_personal_alpha"
                    | "verified_thread_scoped_control"
                    | "strong_process_containment"
            )
            || !matches!(self.native_subagent_policy.as_str(), "enabled" | "disabled")
            || (self.execution_lane == "shared_readonly"
                && (self.requested_access != "read_only"
                    || self.native_subagent_policy != "disabled"))
            || self.required_capabilities.len() > 64
            || self
                .required_capabilities
                .iter()
                .any(|capability| !bounded(capability, 128))
            || self
                .required_capabilities
                .windows(2)
                .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
        {
            return Err(MachineError::invalid_argument(
                "agent_configuration",
                "invalid admitted configuration identity",
            ));
        }
        domain_digest(CONFIGURATION_SCHEMA, self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRef {
    pub kind: String,
    pub identity: String,
    pub content: BytesRef,
    pub media_type: String,
}

impl ContextRef {
    fn validate(&self) -> Result<(), MachineError> {
        if !matches!(
            self.kind.as_str(),
            "accepted_context" | "artifact" | "review_target"
        ) || !opaque(&self.identity)
            || !bounded(&self.media_type, 128)
        {
            return Err(MachineError::invalid_argument(
                "context_ref",
                "invalid context reference",
            ));
        }
        self.content
            .validate(0, MAX_JCS_SAFE_INTEGER)
            .map_err(|_| MachineError::invalid_argument("context_ref", "invalid content reference"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRef {
    pub content: BytesRef,
    pub media_type: String,
    pub detail: String,
}

impl ImageRef {
    fn validate(&self) -> Result<(), MachineError> {
        if !matches!(
            self.media_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp" | "image/gif"
        ) || !matches!(self.detail.as_str(), "auto" | "low" | "high")
        {
            return Err(MachineError::invalid_argument(
                "image_ref",
                "invalid image reference",
            ));
        }
        self.content
            .validate(0, MAX_JCS_SAFE_INTEGER)
            .map_err(|_| MachineError::invalid_argument("image_ref", "invalid content reference"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedTask {
    pub schema: String,
    pub purpose: String,
    pub task_body_schema: String,
    pub task_body: BytesRef,
    pub contexts: Vec<ContextRef>,
    pub images: Vec<ImageRef>,
    pub expected_output_schema: String,
    pub execution_intent: String,
    pub admitted_model: String,
    pub admitted_effort: String,
    pub role_snapshot_digest: String,
    pub agent_configuration_digest: String,
}

impl AcceptedTask {
    pub fn digest(&self) -> Result<String, MachineError> {
        if self.schema != ACCEPTED_TASK_SCHEMA
            || !bounded(&self.purpose, 64)
            || !bounded(&self.task_body_schema, 256)
            || self.contexts.len() > 64
            || self.images.len() > 64
            || !bounded(&self.expected_output_schema, 256)
            || !matches!(
                self.execution_intent.as_str(),
                "read_only" | "canonical_workspace_write"
            )
            || !bounded(&self.admitted_model, 256)
            || !bounded(&self.admitted_effort, 128)
            || !prefixed_digest(&self.role_snapshot_digest)
            || !prefixed_digest(&self.agent_configuration_digest)
        {
            return Err(MachineError::invalid_argument(
                "accepted_task",
                "invalid accepted task identity",
            ));
        }
        self.task_body
            .validate(1, MAX_JCS_SAFE_INTEGER)
            .map_err(|_| {
                MachineError::invalid_argument("accepted_task", "invalid task body reference")
            })?;
        for context in &self.contexts {
            context.validate()?;
        }
        for image in &self.images {
            image.validate()?;
        }
        domain_digest(ACCEPTED_TASK_SCHEMA, self)
    }
}

pub const COMPLETE_RESULT_SCHEMA: &str = "dolgorae.complete-result/v1";

fn safe_code(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_uppercase())
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
        && value.len() <= 128
}

/// Frozen pattern: no leading slash, no `..` segment, no ECMAScript line terminator.
/// `maxLength` is 8192 scalars. This definition publishes no byte ceiling.
fn safe_relative_path(value: &str) -> bool {
    (1..=8192).contains(&value.chars().count())
        && !value.starts_with('/')
        && !value
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
        && !value.split('/').any(|segment| segment == "..")
}

/// The closed complete-result change ceiling (MDC:465, :481).
pub(crate) const MAX_COMPLETE_RESULT_CHANGES: usize = 20_000;

/// Whether one change path can be carried by the closed `ChangeRef`.
///
/// The producer checks this while it still can publish the typed unavailable
/// result, instead of leaving a settlement that fails the closed validation
/// forever.
pub(crate) fn representable_change_path(value: &str) -> bool {
    safe_relative_path(value)
}

fn change_key(change: &ChangeRef) -> (&[u8], Option<&[u8]>, &[u8]) {
    (
        change.path.as_bytes(),
        change.old_path.as_deref().map(str::as_bytes),
        change.kind.as_bytes(),
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultSource {
    pub scope_kind: String,
    pub scope_ref: String,
    pub worker_ref: String,
    pub task_ref: String,
    pub role_snapshot_digest: String,
    pub agent_configuration_digest: String,
    pub accepted_task_digest: String,
}

impl ResultSource {
    fn validate(&self) -> Result<(), MachineError> {
        if !matches!(
            self.scope_kind.as_str(),
            "external_engagement" | "orchestrated_session" | "workspace" | "run" | "profile"
        ) || !opaque(&self.scope_ref)
            || !opaque(&self.worker_ref)
            || !opaque(&self.task_ref)
            || !prefixed_digest(&self.role_snapshot_digest)
            || !prefixed_digest(&self.agent_configuration_digest)
            || !prefixed_digest(&self.accepted_task_digest)
        {
            return Err(MachineError::invalid_argument(
                "result_source",
                "invalid result source",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeRef {
    pub kind: String,
    pub path: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub old_path: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub before: Option<BytesRef>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub after: Option<BytesRef>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub before_mode: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub after_mode: Option<u64>,
}

impl ChangeRef {
    fn validate(&self) -> Result<(), MachineError> {
        let content = |bytes: &BytesRef| bytes.validate(0, 8_388_608).is_ok();
        let mode = |value: Option<u64>| value.is_none_or(|value| value <= MAX_JCS_SAFE_INTEGER);
        let shaped = match self.kind.as_str() {
            "add" => {
                self.old_path.is_none()
                    && self.before.is_none()
                    && self.before_mode.is_none()
                    && self.after.as_ref().is_some_and(content)
                    && mode(self.after_mode)
            }
            "delete" => {
                self.old_path.is_none()
                    && self.after.is_none()
                    && self.after_mode.is_none()
                    && self.before.as_ref().is_some_and(content)
                    && mode(self.before_mode)
            }
            "modify" => {
                self.old_path.is_none()
                    && self.before.as_ref().is_some_and(content)
                    && self.after.as_ref().is_some_and(content)
                    && mode(self.before_mode)
                    && mode(self.after_mode)
            }
            "rename" => {
                self.old_path.as_deref().is_some_and(safe_relative_path)
                    && self.before.as_ref().is_some_and(content)
                    && self.after.as_ref().is_some_and(content)
                    && mode(self.before_mode)
                    && mode(self.after_mode)
            }
            _ => false,
        };
        if safe_relative_path(&self.path) && shaped {
            Ok(())
        } else {
            Err(MachineError::invalid_argument(
                "change_ref",
                "invalid change reference",
            ))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteResult {
    pub schema: String,
    pub source: ResultSource,
    pub terminal_outcome: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub final_response: Option<BytesRef>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub structured_output_schema: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub structured_output: Option<BytesRef>,
    pub changes: Vec<ChangeRef>,
    pub attachments: Vec<ContextRef>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub safe_failure_code: Option<String>,
}

impl CompleteResult {
    pub fn digest(&self) -> Result<String, MachineError> {
        if self.schema != COMPLETE_RESULT_SCHEMA
            || self.changes.len() > MAX_COMPLETE_RESULT_CHANGES
            || self.attachments.len() > MAX_COMPLETE_RESULT_CHANGES
        {
            return Err(MachineError::invalid_argument(
                "complete_result",
                "invalid complete result identity",
            ));
        }
        self.source.validate()?;
        for change in &self.changes {
            change.validate()?;
        }
        for attachment in &self.attachments {
            attachment.validate()?;
        }
        if let Some(response) = &self.final_response {
            response.validate(0, 33_554_432).map_err(|_| {
                MachineError::invalid_argument(
                    "complete_result",
                    "invalid final response reference",
                )
            })?;
        }
        match (&self.structured_output_schema, &self.structured_output) {
            (None, None) => {}
            (Some(schema), Some(output)) if bounded(schema, 256) => {
                output.validate(0, MAX_JCS_SAFE_INTEGER).map_err(|_| {
                    MachineError::invalid_argument(
                        "complete_result",
                        "invalid structured output reference",
                    )
                })?;
            }
            _ => {
                return Err(MachineError::invalid_argument(
                    "complete_result",
                    "invalid complete result identity",
                ));
            }
        }
        let failure = match self.terminal_outcome.as_str() {
            "completed" => self.safe_failure_code.is_none(),
            "failed" | "cancelled" | "expired" => {
                self.safe_failure_code.as_deref().is_none_or(safe_code)
            }
            _ => false,
        };
        if !failure
            || self
                .changes
                .windows(2)
                .any(|pair| change_key(&pair[0]) >= change_key(&pair[1]))
            || self.attachments.windows(2).any(|pair| {
                (pair[0].kind.as_bytes(), pair[0].identity.as_bytes())
                    >= (pair[1].kind.as_bytes(), pair[1].identity.as_bytes())
            })
        {
            return Err(MachineError::invalid_argument(
                "complete_result",
                "invalid complete result identity",
            ));
        }
        domain_digest(COMPLETE_RESULT_SCHEMA, self)
    }
}

const DELEGATION_IDENTITY_SCHEMA: &str = "dolgorae.delegation-identity/v1";
const MEMBER_RELEASE_PROOF_SCHEMA: &str = "dolgorae.member-release-proof/v1";
pub(crate) const MEMBER_RELEASE_OPERATION_SCHEMA: &str = "dolgorae.member-release-operation/v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerScope {
    pub binding_ref: String,
    pub podway_workspace_ref: String,
    pub podway_session_ref: String,
    pub podway_attempt_ref: String,
    pub delegated_placement_ref: String,
    pub max_rounds: u64,
}

impl CallerScope {
    pub fn validate(&self) -> Result<(), MachineError> {
        if [
            &self.binding_ref,
            &self.podway_workspace_ref,
            &self.podway_session_ref,
            &self.podway_attempt_ref,
            &self.delegated_placement_ref,
        ]
        .iter()
        .all(|value| opaque(value))
            && (1..=32).contains(&self.max_rounds)
        {
            Ok(())
        } else {
            Err(MachineError::invalid_argument(
                "caller_scope",
                "invalid caller scope",
            ))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberReleaseProof {
    pub schema: String,
    pub operation_ref: String,
    pub operation_kind: String,
    pub workspace_ref: String,
    pub scope_kind: String,
    pub scope_ref: String,
    pub member_ref: String,
    pub run_ref: String,
    pub controller_principal: String,
    pub controller_generation: u64,
    pub terminal_membership_revision: u64,
    pub settled_workload_revision: u64,
}

impl MemberReleaseProof {
    pub fn validate(&self) -> Result<(), MachineError> {
        if self.schema == MEMBER_RELEASE_PROOF_SCHEMA
            && uuid7(&self.operation_ref)
            && release_pair(&self.operation_kind, &self.scope_kind)
            && [
                &self.workspace_ref,
                &self.scope_ref,
                &self.member_ref,
                &self.run_ref,
                &self.controller_principal,
            ]
            .iter()
            .all(|value| opaque(value))
            && (1..=MAX_JCS_SAFE_INTEGER).contains(&self.controller_generation)
            && self.terminal_membership_revision <= MAX_JCS_SAFE_INTEGER
            && self.settled_workload_revision <= MAX_JCS_SAFE_INTEGER
        {
            Ok(())
        } else {
            Err(MachineError::invalid_argument(
                "member_release_proof",
                "invalid member release proof",
            ))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispositionProof {
    pub state: String,
    pub observed_after_result_digest: String,
    pub membership_revision: u64,
    pub workload_revision: u64,
    pub running: u64,
    pub queued: u64,
    pub claimed: u64,
    pub dispatching: u64,
    pub unresolved: u64,
    pub pending_interaction: bool,
    pub cleanup_pending: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub release_receipt: Option<MemberReleaseProof>,
}

impl DispositionProof {
    pub fn validate(&self) -> Result<(), MachineError> {
        if !prefixed_digest(&self.observed_after_result_digest)
            || self.membership_revision > MAX_JCS_SAFE_INTEGER
            || self.workload_revision > MAX_JCS_SAFE_INTEGER
            || self.running != 0
            || self.queued != 0
            || self.claimed != 0
            || self.dispatching != 0
            || self.unresolved != 0
            || self.pending_interaction
            || self.cleanup_pending
        {
            return Err(MachineError::invalid_argument(
                "disposition_proof",
                "invalid disposition proof",
            ));
        }
        match self.state.as_str() {
            "idle" if self.release_receipt.is_none() => Ok(()),
            "released" => self
                .release_receipt
                .as_ref()
                .ok_or_else(|| {
                    MachineError::invalid_argument("disposition_proof", "invalid disposition proof")
                })?
                .validate(),
            _ => Err(MachineError::invalid_argument(
                "disposition_proof",
                "invalid disposition proof",
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberReleaseOperation {
    pub schema: String,
    pub operation_ref: String,
    pub operation_kind: String,
    pub workspace_ref: String,
    pub scope_kind: String,
    pub scope_ref: String,
    pub member_ref: String,
    pub run_ref: String,
    pub controller_principal: String,
    pub controller_generation: u64,
    pub admitted_membership_revision: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub owner_ref: Option<String>,
    pub progress: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub safe_failure_code: Option<String>,
    pub required_action: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub terminal_membership_revision: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub settled_workload_revision: Option<u64>,
}

impl MemberReleaseOperation {
    pub fn validate(&self) -> Result<(), MachineError> {
        let terminal = self
            .terminal_membership_revision
            .is_some_and(|revision| revision <= MAX_JCS_SAFE_INTEGER);
        let settled = self
            .settled_workload_revision
            .is_some_and(|revision| revision <= MAX_JCS_SAFE_INTEGER);
        let open = self.terminal_membership_revision.is_none()
            && self.settled_workload_revision.is_none()
            && self.safe_failure_code.is_none()
            && self.required_action == "none";
        let progress = match self.progress.as_str() {
            "requested" | "settling" => open,
            "terminal" => {
                terminal
                    && settled
                    && self.safe_failure_code.is_none()
                    && self.required_action == "none"
            }
            "cleanup_failed" | "recovery_needed" => {
                self.terminal_membership_revision.is_none()
                    && self.settled_workload_revision.is_none()
                    && self.safe_failure_code.as_deref().is_some_and(safe_code)
                    && matches!(
                        self.required_action.as_str(),
                        "reconcile_operation" | "recover_run" | "restore_state"
                    )
            }
            _ => false,
        };
        if self.schema == MEMBER_RELEASE_OPERATION_SCHEMA
            && uuid7(&self.operation_ref)
            && release_pair(&self.operation_kind, &self.scope_kind)
            && [
                &self.workspace_ref,
                &self.scope_ref,
                &self.member_ref,
                &self.run_ref,
                &self.controller_principal,
            ]
            .iter()
            .all(|value| opaque(value))
            && self.owner_ref.as_ref().is_none_or(|owner| opaque(owner))
            && (1..=MAX_JCS_SAFE_INTEGER).contains(&self.controller_generation)
            && self.admitted_membership_revision <= MAX_JCS_SAFE_INTEGER
            && progress
        {
            Ok(())
        } else {
            Err(MachineError::invalid_argument(
                "member_release_operation",
                "invalid member release operation",
            ))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationIdentity {
    pub schema: String,
    pub mapping_contract: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub binding_ref: String,
    pub primary_host: String,
    pub scope_kind: String,
    pub scope_ref: String,
    pub supervisor_ref: String,
    pub worker_ref: String,
    pub task_ref: String,
    pub role_ref: String,
    pub role_body_sha256: String,
    pub role_body_byte_length: u64,
    pub role_snapshot_digest: String,
    pub instruction_identity_digest: String,
    pub agent_configuration_digest: String,
    pub accepted_task_digest: String,
    pub result_digest: String,
    pub complete_result: CompleteResult,
    pub caller_scope: CallerScope,
    pub rounds_used: u64,
    pub accepted_assignments: u64,
    pub unresolved_assignments: u64,
    pub worker_disposition: String,
    pub disposition_proof: DispositionProof,
}

impl DelegationIdentity {
    pub fn validate(&self) -> Result<(), MachineError> {
        let release_kind = match (
            self.mapping_contract.as_str(),
            self.primary_host.as_str(),
            self.scope_kind.as_str(),
        ) {
            ("dolgorae.podway-external/v1", "external", "external_engagement") => {
                "release_external_specialist"
            }
            ("dolgorae.podway-managed/v1", "managed", "orchestrated_session") => {
                "release_specialist"
            }
            _ => {
                return Err(MachineError::invalid_argument(
                    "delegation_identity",
                    "invalid delegation identity",
                ));
            }
        };
        if self.schema != DELEGATION_IDENTITY_SCHEMA
            || !bounded(&self.adapter_id, 128)
            || !bounded(&self.adapter_version, 128)
            || !opaque(&self.binding_ref)
            || !opaque(&self.scope_ref)
            || !opaque(&self.supervisor_ref)
            || !opaque(&self.worker_ref)
            || !opaque(&self.task_ref)
            || !opaque(&self.role_ref)
            || !raw_sha256(&self.role_body_sha256)
            || !(1..=65_536).contains(&self.role_body_byte_length)
            || !prefixed_digest(&self.role_snapshot_digest)
            || !prefixed_digest(&self.instruction_identity_digest)
            || !prefixed_digest(&self.agent_configuration_digest)
            || !prefixed_digest(&self.accepted_task_digest)
            || !prefixed_digest(&self.result_digest)
            || !(1..=32).contains(&self.rounds_used)
            || self.accepted_assignments > 32
            || self.unresolved_assignments > 32
            || self.accepted_assignments + self.unresolved_assignments != self.rounds_used
        {
            return Err(MachineError::invalid_argument(
                "delegation_identity",
                "invalid delegation identity",
            ));
        }
        self.caller_scope.validate()?;
        self.disposition_proof.validate()?;
        if self.caller_scope.binding_ref != self.binding_ref
            || self.rounds_used > self.caller_scope.max_rounds
            || self.disposition_proof.state != self.worker_disposition
            || self.disposition_proof.observed_after_result_digest != self.result_digest
        {
            return Err(MachineError::invalid_argument(
                "delegation_identity",
                "invalid delegation identity",
            ));
        }
        if let Some(proof) = &self.disposition_proof.release_receipt
            && (proof.operation_kind != release_kind
                || proof.scope_kind != self.scope_kind
                || proof.scope_ref != self.scope_ref
                || proof.member_ref != self.worker_ref
                || proof.terminal_membership_revision > self.disposition_proof.membership_revision
                || proof.settled_workload_revision > self.disposition_proof.workload_revision)
        {
            return Err(MachineError::invalid_argument(
                "delegation_identity",
                "invalid delegation identity",
            ));
        }
        let source = &self.complete_result.source;
        if source.scope_kind != self.scope_kind
            || source.scope_ref != self.scope_ref
            || source.worker_ref != self.worker_ref
            || source.task_ref != self.task_ref
            || source.role_snapshot_digest != self.role_snapshot_digest
            || source.agent_configuration_digest != self.agent_configuration_digest
            || source.accepted_task_digest != self.accepted_task_digest
        {
            return Err(MachineError::invalid_argument(
                "delegation_identity",
                "invalid delegation identity",
            ));
        }
        if self.complete_result.digest()? != self.result_digest {
            return Err(MachineError::invalid_argument(
                "delegation_identity",
                "invalid delegation identity",
            ));
        }
        Ok(())
    }
}

/// Compare the stated admission, terminal proof, and observed disposition revisions.
/// `terminal == membership` and `settled == workload` are valid. This does not
/// read a stored revision.
pub fn release_revision_order(
    operation: &MemberReleaseOperation,
    proof: &MemberReleaseProof,
    disposition: &DispositionProof,
) -> Result<(), MachineError> {
    let (Some(terminal), Some(settled)) = (
        operation.terminal_membership_revision,
        operation.settled_workload_revision,
    ) else {
        return Err(MachineError::invalid_argument(
            "member_release_operation",
            "invalid member release revision order",
        ));
    };
    if operation.admitted_membership_revision <= MAX_JCS_SAFE_INTEGER
        && terminal <= MAX_JCS_SAFE_INTEGER
        && settled <= MAX_JCS_SAFE_INTEGER
        && disposition.membership_revision <= MAX_JCS_SAFE_INTEGER
        && disposition.workload_revision <= MAX_JCS_SAFE_INTEGER
        && operation.admitted_membership_revision < terminal
        && terminal == proof.terminal_membership_revision
        && terminal <= disposition.membership_revision
        && settled == proof.settled_workload_revision
        && settled <= disposition.workload_revision
    {
        Ok(())
    } else {
        Err(MachineError::invalid_argument(
            "member_release_operation",
            "invalid member release revision order",
        ))
    }
}

fn release_pair(kind: &str, scope: &str) -> bool {
    matches!(
        (kind, scope),
        ("release_external_specialist", "external_engagement")
            | ("release_specialist", "orchestrated_session")
    )
}

fn raw_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn uuid7(value: &str) -> bool {
    uuid::Uuid::try_parse(value).is_ok_and(|id| {
        id.get_variant() == uuid::Variant::RFC4122
            && id.get_version_num() == 7
            && id.to_string() == value
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupNamespace {
    pub controller_principal: String,
    pub controller_generation: u64,
    pub workspace_ref: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub scope_kind: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub scope_ref: Option<String>,
    pub operation_kind: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedSubjects {
    #[serde(deserialize_with = "Option::deserialize")]
    pub run_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub member_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub task_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub group_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub result_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub merge_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub engagement_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub maintenance_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub provisioning_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub turn_ref: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub interaction_ref: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationReceipt {
    pub schema: String,
    pub operation_ref: String,
    pub namespace: LookupNamespace,
    pub accepted_input_digest: String,
    pub accepted_subjects: AcceptedSubjects,
    pub receipt_digest: String,
    pub accepted_revision: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub result_digest: Option<String>,
}

impl OperationReceipt {
    /// The self-verifying digest of this closed receipt.
    ///
    /// The digest covers every closed field except itself, so a reader that
    /// retains only the receipt can recompute it and refuse a tampered record
    /// instead of trusting it.
    pub fn compute_digest(&self) -> Result<String, MachineError> {
        let mut unsigned = self.clone();
        unsigned.receipt_digest = String::new();
        domain_digest("dolgorae.operation-receipt/v1", &unsigned)
    }

    /// Require the retained `receipt_digest` to match its own closed fields.
    pub fn verify_digest(&self) -> Result<(), MachineError> {
        if self.receipt_digest != self.compute_digest()? {
            return Err(MachineError::invalid_argument(
                "operation_receipt",
                "the retained receipt digest does not verify",
            ));
        }
        Ok(())
    }

    /// Validate the closed receipt, not its authority or original acceptance.
    /// The owning store must also bind it to the retained operation namespace.
    pub fn validate(&self) -> Result<(), MachineError> {
        let namespace = &self.namespace;
        let subjects = &self.accepted_subjects;
        let all_subjects = [
            &subjects.run_ref,
            &subjects.member_ref,
            &subjects.task_ref,
            &subjects.group_ref,
            &subjects.result_ref,
            &subjects.merge_ref,
            &subjects.engagement_ref,
            &subjects.maintenance_ref,
            &subjects.provisioning_ref,
            &subjects.turn_ref,
            &subjects.interaction_ref,
        ];
        let required = match namespace.operation_kind.as_str() {
            "start_run" | "fork_run" | "create_write_continuation" => subjects.run_ref.is_some(),
            "submit_turn" => subjects.run_ref.is_some() && subjects.turn_ref.is_some(),
            "resolve_interaction" => {
                subjects.run_ref.is_some()
                    && subjects.turn_ref.is_some()
                    && subjects.interaction_ref.is_some()
            }
            "install_provisioning" => subjects.provisioning_ref.is_some(),
            "open_external_engagement" => subjects.engagement_ref.is_some(),
            "hire_external_specialist" | "spawn_specialist" => subjects.member_ref.is_some(),
            "assign_external_specialist_task" | "assign_specialist_task" | "assign_synthesis" => {
                subjects.task_ref.is_some()
            }
            "assign_group" => subjects.group_ref.is_some(),
            "merge_group_results" => subjects.merge_ref.is_some(),
            _ => false,
        };
        if self.schema != "dolgorae.operation-receipt/v1"
            || !uuid7(&self.operation_ref)
            || !opaque(&namespace.controller_principal)
            || !opaque(&namespace.workspace_ref)
            || !(1..=MAX_JCS_SAFE_INTEGER).contains(&namespace.controller_generation)
            || namespace.scope_kind.as_deref().is_some_and(|kind| {
                !matches!(
                    kind,
                    "external_engagement"
                        | "orchestrated_session"
                        | "workspace"
                        | "run"
                        | "profile"
                )
            })
            || namespace
                .scope_ref
                .as_ref()
                .is_some_and(|reference| !opaque(reference))
            // Nullable-required pair: an aggregate scope names both or neither.
            || namespace.scope_kind.is_some() != namespace.scope_ref.is_some()
            || namespace.idempotency_key.is_empty()
            || namespace.idempotency_key.len() > 256
            || !prefixed_digest(&self.accepted_input_digest)
            || !prefixed_digest(&self.receipt_digest)
            || self.accepted_revision > MAX_JCS_SAFE_INTEGER
            || self
                .result_digest
                .as_ref()
                .is_some_and(|digest| !prefixed_digest(digest))
            || !required
            || all_subjects
                .iter()
                .any(|subject| subject.as_ref().is_some_and(|value| !opaque(value)))
        {
            return Err(MachineError::invalid_argument(
                "operation_receipt",
                "invalid retained operation receipt",
            ));
        }
        Ok(())
    }
}

fn opaque(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
}

/// Bounds for the closed schema fields that carry no character pattern: a
/// non-empty scalar count and a UTF-8 byte ceiling. Both limits are checked
/// because the schemas state `maxLength` and `x-maxUtf8Bytes` together; no
/// control-character rule exists, and adding one would refuse valid objects.
fn bounded(value: &str, maximum: usize) -> bool {
    (1..=maximum).contains(&value.chars().count()) && value.len() <= maximum
}

fn prefixed_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The exact persisted instructions/v3 shape selected by H1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionSnapshot {
    pub schema: String,
    pub core_prefix_version: u32,
    pub core_prefix_sha256: String,
    pub common: SourceCapture,
    pub role: RoleCapture,
    pub composition_sha256: String,
    pub dependencies: Vec<NamedSourceCapture>,
    pub selected_guidance: Vec<NamedSourceCapture>,
    pub role_snapshot: RoleSnapshot,
    pub role_snapshot_digest: String,
    pub instruction_identity: InstructionIdentity,
    pub instruction_identity_digest: String,
}

impl InstructionSnapshot {
    /// Build the closed `dolgorae.instructions/v3` identity from accepted
    /// capture bytes. Capture stays in `instructions`; identity stays here.
    ///
    /// This is the single admission gate for the closed instruction rules:
    /// dependency aliases are sorted by UTF-8 bytes, aliases are unique, and the
    /// 65,536-byte aggregate covers common, Role, dependencies and guidance
    /// (excluding core and separators). Every allocation or dispatch path must
    /// call this before accepting work; raw capture alone proves nothing.
    pub fn from_capture(
        capture: crate::instructions::DelegationCapture,
    ) -> Result<Self, MachineError> {
        Self::compose(
            capture.common,
            capture.role,
            capture
                .dependencies
                .into_iter()
                .map(|source| NamedSourceCapture::new(source.name, &source.body))
                .collect::<Result<Vec<_>, MachineError>>()?,
            capture
                .selected_guidance
                .into_iter()
                .map(|source| NamedSourceCapture::new(source.name, &source.body))
                .collect::<Result<Vec<_>, MachineError>>()?,
        )
    }

    pub fn compose(
        common: SourceCapture,
        role: RoleCapture,
        mut dependencies: Vec<NamedSourceCapture>,
        selected_guidance: Vec<NamedSourceCapture>,
    ) -> Result<Self, MachineError> {
        common.validate()?;
        role.validate()?;
        validate_sources(&dependencies)?;
        validate_sources(&selected_guidance)?;
        dependencies.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
        let source_bytes = common.body.len()
            + role.body.len()
            + dependencies
                .iter()
                .map(|source| source.source.body.len())
                .sum::<usize>()
            + selected_guidance
                .iter()
                .map(|source| source.source.body.len())
                .sum::<usize>();
        if source_bytes > MAX_SOURCE_BYTES {
            return Err(invalid());
        }
        let role_snapshot = RoleSnapshot {
            schema: ROLE_SCHEMA.to_owned(),
            role_body: BytesRef::from_bytes(role.body.as_bytes()),
            dependencies: dependencies
                .iter()
                .map(NamedSourceCapture::reference)
                .collect(),
            selected_guidance: selected_guidance
                .iter()
                .map(NamedSourceCapture::reference)
                .collect(),
        };
        let role_snapshot_digest = role_snapshot.digest()?;
        let composition = compose_bytes(&common, &role, &dependencies, &selected_guidance);
        let instruction_identity = InstructionIdentity {
            schema: INSTRUCTION_SCHEMA.to_owned(),
            core_version: u64::from(CORE_PREFIX_VERSION),
            core: BytesRef::from_bytes(CORE_PREFIX.as_bytes()),
            common: BytesRef::from_bytes(common.body.as_bytes()),
            role_snapshot_digest: role_snapshot_digest.clone(),
            dependency_order: dependencies
                .iter()
                .map(|source| source.name.clone())
                .collect(),
            guidance_order: selected_guidance
                .iter()
                .map(|source| source.name.clone())
                .collect(),
            composition_version: COMPOSITION_VERSION.to_owned(),
            composition: BytesRef::from_bytes(composition.as_bytes()),
        };
        let instruction_identity_digest = instruction_identity.digest()?;
        Ok(Self {
            schema: "dolgorae.instructions/v3".to_owned(),
            core_prefix_version: CORE_PREFIX_VERSION,
            core_prefix_sha256: sha256_hex(CORE_PREFIX.as_bytes()),
            common,
            role,
            composition_sha256: sha256_hex(composition.as_bytes()),
            dependencies,
            selected_guidance,
            role_snapshot,
            role_snapshot_digest,
            instruction_identity,
            instruction_identity_digest,
        })
    }

    /// Recompute every derived field before using retained execution bytes.
    pub fn developer_prefix(&self) -> Result<String, MachineError> {
        let expected = Self::compose(
            self.common.clone(),
            self.role.clone(),
            self.dependencies.clone(),
            self.selected_guidance.clone(),
        )?;
        if expected != *self {
            return Err(invalid());
        }
        Ok(compose_bytes(
            &self.common,
            &self.role,
            &self.dependencies,
            &self.selected_guidance,
        ))
    }

    #[must_use]
    pub fn source_bytes(&self) -> usize {
        self.common.body.len()
            + self.role.body.len()
            + self
                .dependencies
                .iter()
                .chain(&self.selected_guidance)
                .map(|source| source.source.body.len())
                .sum::<usize>()
    }
}

fn compose_bytes(
    common: &SourceCapture,
    role: &RoleCapture,
    dependencies: &[NamedSourceCapture],
    guidance: &[NamedSourceCapture],
) -> String {
    std::iter::once(CORE_PREFIX)
        .chain([common.body.as_str(), role.body.as_str()])
        .chain(
            dependencies
                .iter()
                .chain(guidance)
                .map(|source| source.source.body.as_str()),
        )
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn validate_alias(name: &str) -> Result<(), MachineError> {
    // `$defs/alias`: 1..256 UTF-8 bytes, no slash, backslash, C0 control or DEL.
    // C1 controls (U+0080-U+009F) are permitted by the pattern.
    if name.is_empty()
        || name.len() > 256
        || name
            .bytes()
            .any(|byte| byte < 0x20 || byte == 0x7f || matches!(byte, b'/' | b'\\'))
    {
        return Err(invalid());
    }
    Ok(())
}

fn validate_sources(sources: &[NamedSourceCapture]) -> Result<(), MachineError> {
    if sources.len() > 64 {
        return Err(invalid());
    }
    let mut names = BTreeSet::new();
    for source in sources {
        validate_alias(&source.name)?;
        source.source.validate()?;
        if !names.insert(&source.name) {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(crate) fn domain_digest(domain: &str, value: &impl Serialize) -> Result<String, MachineError> {
    let json = serde_json::to_string(value).map_err(|_| invalid())?;
    let parsed = parse(&json).map_err(|_| invalid())?;
    let canonical = canonicalize(&parsed).map_err(|_| invalid())?;
    let mut bytes = Vec::with_capacity(domain.len() + 1 + canonical.len());
    bytes.extend_from_slice(domain.as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(&canonical);
    Ok(format!("sha256:{}", sha256_hex(&bytes)))
}

fn invalid() -> MachineError {
    MachineError::new(
        "INSTRUCTION_SOURCE_INVALID",
        "captured delegation instructions are invalid",
        false,
        serde_json::json!({"phase": "admission", "effect": "none"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_and_receipt_uuid7_require_rfc_variant_and_canonical_text() {
        let identity: DelegationIdentity = serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ))
        .unwrap();
        let mut proof = identity.disposition_proof.release_receipt.unwrap();
        let mut operation: MemberReleaseOperation = serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-state-release-external-terminal.valid.json"
        ))
        .unwrap();
        let lookup: serde_json::Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-lookup-accepted.valid.json"
        ))
        .unwrap();
        let mut receipt: OperationReceipt =
            serde_json::from_value(lookup["receipt"].clone()).unwrap();
        assert!(proof.validate().is_ok());
        assert!(operation.validate().is_ok());
        assert!(receipt.validate().is_ok());
        let canonical = "01900000-0000-7abc-8000-000000000068";
        for nibble in "0123456789abcdef".chars() {
            let mut value = canonical.to_owned();
            value.replace_range(19..20, &nibble.to_string());
            let allowed = matches!(nibble, '8' | '9' | 'a' | 'b');
            proof.operation_ref = value.clone();
            operation.operation_ref = value.clone();
            receipt.operation_ref = value;
            assert_eq!(proof.validate().is_ok(), allowed, "proof variant {nibble}");
            assert_eq!(
                operation.validate().is_ok(),
                allowed,
                "operation variant {nibble}"
            );
            assert_eq!(
                receipt.validate().is_ok(),
                allowed,
                "receipt variant {nibble}"
            );
        }
        for value in [
            canonical.to_uppercase(),
            canonical.replace('-', ""),
            format!("urn:uuid:{canonical}"),
            format!("{{{canonical}}}"),
            canonical.replacen("7abc", "4abc", 1),
        ] {
            proof.operation_ref = value.clone();
            operation.operation_ref = value.clone();
            receipt.operation_ref = value;
            assert!(proof.validate().is_err());
            assert!(operation.validate().is_err());
            assert!(receipt.validate().is_err());
        }
    }

    /// A real accepted K receipt is not a member release proof — the
    /// closed type refuses it wherever `member-release-proof/v1` is required —
    /// and the released schema branch without its receipt is refused (branch 4).
    #[test]
    fn a_keyed_receipt_is_not_a_release_proof_and_released_requires_one() {
        let lookup: serde_json::Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-lookup-accepted.valid.json"
        ))
        .unwrap();
        let receipt: OperationReceipt = serde_json::from_value(lookup["receipt"].clone()).unwrap();
        receipt.validate().unwrap();
        // A genuine hire/assign receipt presented where a release proof is
        // required never parses as one (closed shape), and no proof validates
        // under the release schema.
        let presented = serde_json::to_value(&receipt).unwrap();
        assert!(serde_json::from_value::<MemberReleaseProof>(presented).is_err());
        let identity: DelegationIdentity = serde_json::from_str(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ))
        .unwrap();
        let mut foreign = identity.disposition_proof.clone();
        foreign.release_receipt = None;
        assert!(foreign.validate().is_err(), "released refuses a null proof");
        let proof = identity.disposition_proof.release_receipt.clone().unwrap();
        let mut mismatched = proof.clone();
        mismatched.operation_kind = "hire_external_specialist".to_owned();
        assert!(mismatched.validate().is_err());
        let mut unpaired = proof.clone();
        unpaired.scope_kind = "orchestrated_session".to_owned();
        assert!(unpaired.validate().is_err());
    }

    fn result_vector() -> serde_json::Value {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|vector| vector["id"] == "result-base")
            .unwrap()["input"]
            .clone()
    }

    #[test]
    fn complete_result_uses_frozen_positive_and_negative_vectors() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        let mut count = 0;
        for vector in vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|vector| vector["domain"] == COMPLETE_RESULT_SCHEMA)
        {
            let result: CompleteResult = serde_json::from_value(vector["input"].clone()).unwrap();
            assert_eq!(result.digest().unwrap(), vector["expected_digest"]);
            count += 1;
        }
        assert_eq!(count, 3);
        let mut count = 0;
        for vector in vectors["invalid"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|vector| vector["domain"] == COMPLETE_RESULT_SCHEMA)
        {
            let parsed =
                serde_json::from_str::<CompleteResult>(vector["raw_json"].as_str().unwrap());
            assert!(
                parsed.is_err() || parsed.unwrap().digest().is_err(),
                "{}",
                vector["id"]
            );
            count += 1;
        }
        assert!(count > 0);
    }

    #[test]
    fn complete_result_retains_nullable_fields_and_complete_content_bounds() {
        let value = result_vector();
        let mut result: CompleteResult = serde_json::from_value(value.clone()).unwrap();
        for length in [0, 65_537, 33_554_432] {
            result.final_response.as_mut().unwrap().byte_length = length;
            assert!(result.digest().is_ok());
        }
        result.final_response.as_mut().unwrap().byte_length = 33_554_433;
        assert!(result.digest().is_err());
        result.final_response = None;
        result.structured_output_schema = Some("schema".to_owned());
        assert!(result.digest().is_err());
        result.structured_output = Some(BytesRef::from_bytes(b"{}"));
        result.structured_output.as_mut().unwrap().byte_length = 65_537;
        assert!(result.digest().is_ok());
        result.structured_output_schema = None;
        assert!(result.digest().is_err());
        result.structured_output = None;
        result.safe_failure_code = Some("FINDINGS_OUTPUT_INVALID".to_owned());
        assert!(result.digest().is_err());
        result.terminal_outcome = "failed".to_owned();
        assert!(result.digest().is_ok());
        result.safe_failure_code = Some("invalid".to_owned());
        assert!(result.digest().is_err());
        result.safe_failure_code = None;
        assert!(result.digest().is_ok());
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove("structured_output");
        assert!(serde_json::from_value::<CompleteResult>(missing).is_err());
        let mut unknown = value.clone();
        unknown["receipt"] = serde_json::json!("not-result-content");
        assert!(serde_json::from_value::<CompleteResult>(unknown).is_err());
        let mut float = value.clone();
        float["changes"][0]["before_mode"] = serde_json::json!(33188.0);
        assert!(serde_json::from_value::<CompleteResult>(float).is_err());
        let duplicate = serde_json::to_string(&value).unwrap().replacen(
            '{',
            "{\"schema\":\"dolgorae.complete-result/v1\",",
            1,
        );
        assert!(serde_json::from_str::<CompleteResult>(&duplicate).is_err());
    }

    #[test]
    fn complete_result_validates_change_shape_and_keeps_sorted_provenance() {
        let mut result: CompleteResult = serde_json::from_value(result_vector()).unwrap();
        let mut change = result.changes[0].clone();
        change.before_mode = None;
        change.after_mode = None;
        assert!(change.validate().is_ok());
        change.old_path = Some("../escape".to_owned());
        assert!(change.validate().is_err());
        change.kind = "modify".to_owned();
        change.old_path = None;
        assert!(change.validate().is_ok());
        change.kind = "add".to_owned();
        assert!(change.validate().is_err());
        change.before = None;
        assert!(change.validate().is_ok());
        change.kind = "delete".to_owned();
        assert!(change.validate().is_err());
        change.before = change.after.take();
        assert!(change.validate().is_ok());
        change.path = "a/../b".to_owned();
        assert!(change.validate().is_err());
        assert!(safe_relative_path(&"é".repeat(8192)));
        assert!(!safe_relative_path(&"é".repeat(8193)));
        result.changes.push(result.changes[0].clone());
        assert!(result.digest().is_err());
        result.changes.pop();
        assert!(result.digest().is_ok());
        change.path = "z".to_owned();
        result.changes.push(change);
        assert!(result.digest().is_ok());
        result.changes.swap(0, 1);
        assert!(result.digest().is_err());
        result.changes.clear();
        let accepted: AcceptedTask = serde_json::from_value(accepted_vector()).unwrap();
        result.attachments = accepted.contexts;
        result.attachments.sort_by(|a, b| {
            (a.kind.as_bytes(), a.identity.as_bytes())
                .cmp(&(b.kind.as_bytes(), b.identity.as_bytes()))
        });
        assert!(result.digest().is_ok());
        result.attachments.swap(0, 1);
        assert!(result.digest().is_err());
    }

    fn accepted_vector() -> serde_json::Value {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|vector| vector["id"] == "task-base")
            .unwrap()["input"]
            .clone()
    }

    #[test]
    fn frozen_accepted_task_vectors_use_the_complete_closed_object() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        let mut count = 0;
        for vector in vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|vector| vector["domain"] == ACCEPTED_TASK_SCHEMA)
        {
            let task: AcceptedTask = serde_json::from_value(vector["input"].clone()).unwrap();
            assert_eq!(task.digest().unwrap(), vector["expected_digest"]);
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn accepted_task_keeps_rich_body_range_order_and_required_fields() {
        let value = accepted_vector();
        let mut task: AcceptedTask = serde_json::from_value(value.clone()).unwrap();
        let first = task.digest().unwrap();
        task.contexts.swap(0, 1);
        assert_ne!(first, task.digest().unwrap());
        task.task_body.byte_length = 65_537;
        assert!(task.digest().is_ok());
        task.task_body.byte_length = MAX_JCS_SAFE_INTEGER;
        assert!(task.digest().is_ok());
        task.task_body.byte_length += 1;
        assert!(task.digest().is_err());
        task.task_body.byte_length = 0;
        assert!(task.digest().is_err());
        task.task_body.byte_length = 1;
        task.contexts = vec![task.contexts[0].clone(); 65];
        assert!(task.digest().is_err());
        let mut unknown = value.clone();
        unknown["envelope_id"] = serde_json::json!("not-task-content");
        assert!(serde_json::from_value::<AcceptedTask>(unknown).is_err());
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove("contexts");
        assert!(serde_json::from_value::<AcceptedTask>(missing).is_err());
        let mut float = value.clone();
        float["task_body"]["byte_length"] = serde_json::json!(1.0);
        assert!(serde_json::from_value::<AcceptedTask>(float).is_err());
        let duplicate = serde_json::to_string(&value).unwrap().replacen(
            '{',
            "{\"schema\":\"dolgorae.accepted-task/v1\",",
            1,
        );
        assert!(serde_json::from_str::<AcceptedTask>(&duplicate).is_err());
    }

    #[test]
    fn accepted_task_preserves_declared_string_and_image_bounds() {
        let mut task: AcceptedTask = serde_json::from_value(accepted_vector()).unwrap();
        task.purpose = "é".repeat(32);
        task.contexts[0].media_type = "application/json\n".to_owned();
        task.contexts[0].identity = "ref\u{0085}".to_owned();
        assert!(task.digest().is_ok());
        task.purpose.push('é');
        assert!(task.digest().is_err());
        task.purpose = "accepted".to_owned();
        task.contexts[0].identity.push('\u{007f}');
        assert!(task.digest().is_err());
        task.contexts[0].identity = "ref".to_owned();
        task.images.push(ImageRef {
            content: BytesRef::from_bytes(b""),
            media_type: "image/png".to_owned(),
            detail: "high".to_owned(),
        });
        assert!(task.digest().is_ok());
        task.images[0].detail = "medium".to_owned();
        assert!(task.digest().is_err());
        task.images[0].detail = "high".to_owned();
        task.images[0].media_type = "image/tiff".to_owned();
        assert!(task.digest().is_err());
    }
    use serde_json::{Value, json};

    fn role(body: &str) -> RoleCapture {
        let source = SourceCapture::from_body(body).unwrap();
        RoleCapture {
            name: "reviewer".to_owned(),
            body: source.body,
            byte_length: source.byte_length,
            sha256: source.sha256,
        }
    }

    fn sample() -> InstructionSnapshot {
        InstructionSnapshot::compose(
            SourceCapture::from_body(" common\r\n").unwrap(),
            role("é e\u{301}\n"),
            vec![
                NamedSourceCapture::new("z".to_owned(), "second").unwrap(),
                NamedSourceCapture::new("a".to_owned(), "first").unwrap(),
            ],
            vec![
                NamedSourceCapture::new("g2".to_owned(), "g2\r\n").unwrap(),
                NamedSourceCapture::new("g1".to_owned(), "g1\n").unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn frozen_role_and_instruction_vectors_use_the_production_digest() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        for vector in vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|vector| {
                matches!(
                    vector["domain"].as_str(),
                    Some(ROLE_SCHEMA | INSTRUCTION_SCHEMA)
                )
            })
        {
            let digest = match vector["domain"].as_str().unwrap() {
                ROLE_SCHEMA => serde_json::from_value::<RoleSnapshot>(vector["input"].clone())
                    .unwrap()
                    .digest(),
                INSTRUCTION_SCHEMA => {
                    serde_json::from_value::<InstructionIdentity>(vector["input"].clone())
                        .unwrap()
                        .digest()
                }
                _ => unreachable!(),
            }
            .unwrap();
            assert_eq!(digest, vector["expected_digest"]);
        }
    }

    #[test]
    fn frozen_negative_vectors_are_rejected_by_the_typed_identity_boundary() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        let mut checked = 0;
        for vector in vectors["invalid"].as_array().unwrap() {
            let raw = vector["raw_json"].as_str().unwrap();
            let result = match vector["domain"].as_str().unwrap() {
                ROLE_SCHEMA => serde_json::from_str::<RoleSnapshot>(raw)
                    .map_err(|_| invalid())
                    .and_then(|value| value.digest()),
                INSTRUCTION_SCHEMA => serde_json::from_str::<InstructionIdentity>(raw)
                    .map_err(|_| invalid())
                    .and_then(|value| value.digest()),
                CONFIGURATION_SCHEMA => serde_json::from_str::<AdmittedConfiguration>(raw)
                    .map_err(|_| invalid())
                    .and_then(|value| value.digest()),
                ACCEPTED_TASK_SCHEMA => serde_json::from_str::<AcceptedTask>(raw)
                    .map_err(|_| invalid())
                    .and_then(|value| value.digest()),
                COMPLETE_RESULT_SCHEMA => serde_json::from_str::<CompleteResult>(raw)
                    .map_err(|_| invalid())
                    .and_then(|value| value.digest()),
                _ => continue,
            };
            assert!(result.is_err(), "accepted invalid vector {}", vector["id"]);
            checked += 1;
        }
        // 20 of the 21 in-subset negative vectors; the mutation-envelope one is
        // exercised by `shared_mutation_envelope_producer_keeps_its_frozen_vectors`.
        assert_eq!(checked, 20);
    }

    #[test]
    fn admitted_configuration_uses_frozen_vectors_and_closed_required_fields() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        let vector = vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|vector| vector["id"] == "configuration-base")
            .unwrap();
        let admitted: AdmittedConfiguration =
            serde_json::from_value(vector["input"].clone()).unwrap();
        assert_eq!(admitted.digest().unwrap(), vector["expected_digest"]);
        let mut value = vector["input"].clone();
        value.as_object_mut().unwrap().remove("purpose_label");
        assert!(serde_json::from_value::<AdmittedConfiguration>(value).is_err());
        let mut value = vector["input"].clone();
        value["writer_generation"] = json!(1);
        assert!(serde_json::from_value::<AdmittedConfiguration>(value).is_err());
        let mut changed = admitted.clone();
        changed.required_capabilities.reverse();
        assert!(changed.digest().is_err());
        let mut changed = admitted.clone();
        changed.execution_lane = "shared_readonly".to_owned();
        changed.requested_access = "canonical_workspace_write".to_owned();
        assert!(changed.digest().is_err());
        changed.requested_access = "read_only".to_owned();
        changed.native_subagent_policy = "enabled".to_owned();
        assert!(changed.digest().is_err());
        changed.native_subagent_policy = "disabled".to_owned();
        assert!(changed.digest().is_ok());
    }

    #[test]
    fn captures_reconstruct_exact_bytes_and_keep_guidance_order() {
        let capture = sample();
        assert_eq!(
            capture.developer_prefix().unwrap(),
            format!(
                "{CORE_PREFIX}\n\n common\r\n\n\né e\u{301}\n\n\nfirst\n\nsecond\n\ng2\r\n\n\ng1\n"
            )
        );
        assert_eq!(capture.instruction_identity.dependency_order, ["a", "z"]);
        assert_eq!(capture.instruction_identity.guidance_order, ["g2", "g1"]);
        let bytes = serde_json::to_vec(&capture).unwrap();
        let restored: InstructionSnapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            restored.developer_prefix().unwrap(),
            capture.developer_prefix().unwrap()
        );
        assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
    }

    #[test]
    fn source_and_derived_identity_tampering_cannot_reach_execution() {
        let original = sample();
        let mut changed = original.clone();
        changed.role.body.push('x');
        assert!(changed.developer_prefix().is_err());
        let mut changed = original.clone();
        changed.dependencies.reverse();
        assert!(changed.developer_prefix().is_err());
        let mut changed = original.clone();
        changed.selected_guidance.reverse();
        assert!(changed.developer_prefix().is_err());
        let mut value = serde_json::to_value(&original).unwrap();
        value["instruction_identity"]["composition"]["byte_length"] = json!(1);
        assert!(
            serde_json::from_value::<InstructionSnapshot>(value)
                .unwrap()
                .developer_prefix()
                .is_err()
        );
        let mut value = serde_json::to_value(&original).unwrap();
        value["source_path"] = json!("never-follow-this");
        assert!(serde_json::from_value::<InstructionSnapshot>(value).is_err());
    }

    #[test]
    fn source_ceiling_excludes_core_and_separators_and_counts_utf8_bytes() {
        let body = "é".repeat((MAX_SOURCE_BYTES - 2) / 2);
        let capture = InstructionSnapshot::compose(
            SourceCapture::from_body("a").unwrap(),
            role(&body),
            vec![NamedSourceCapture::new("d".to_owned(), "b").unwrap()],
            vec![],
        )
        .unwrap();
        assert_eq!(capture.source_bytes(), MAX_SOURCE_BYTES);
        assert!(capture.developer_prefix().unwrap().len() > MAX_SOURCE_BYTES);
        assert!(
            InstructionSnapshot::compose(
                SourceCapture::from_body("aa").unwrap(),
                role(&body),
                vec![NamedSourceCapture::new("d".to_owned(), "b").unwrap()],
                vec![]
            )
            .is_err()
        );
    }

    #[test]
    fn aliases_duplicates_empty_nul_and_collection_bounds_fail_closed() {
        for name in ["", "a/b", "a\\b", "bad\nname"] {
            assert!(NamedSourceCapture::new(name.to_owned(), "body").is_err());
        }
        assert!(SourceCapture::from_body("").is_err());
        assert!(SourceCapture::from_body("a\0b").is_err());
        let source = NamedSourceCapture::new("same".to_owned(), "body").unwrap();
        assert!(
            InstructionSnapshot::compose(
                SourceCapture::from_body("common").unwrap(),
                role("role"),
                vec![source.clone(), source],
                vec![]
            )
            .is_err()
        );
        let sources = (0..65)
            .map(|index| NamedSourceCapture::new(index.to_string(), "b").unwrap())
            .collect();
        assert!(
            InstructionSnapshot::compose(
                SourceCapture::from_body("c").unwrap(),
                role("r"),
                sources,
                vec![]
            )
            .is_err()
        );
    }

    #[test]
    fn display_name_is_attribution_while_content_and_guidance_bind_identity() {
        let original = sample();
        let mut role = original.role.clone();
        role.name = "general".to_owned();
        let renamed = InstructionSnapshot::compose(
            original.common.clone(),
            role,
            original.dependencies.clone(),
            original.selected_guidance.clone(),
        )
        .unwrap();
        assert_eq!(renamed.role_snapshot_digest, original.role_snapshot_digest);
        let mut guidance = original.selected_guidance.clone();
        guidance.reverse();
        let reordered = InstructionSnapshot::compose(
            original.common,
            original.role,
            original.dependencies,
            guidance,
        )
        .unwrap();
        assert_ne!(
            reordered.role_snapshot_digest,
            original.role_snapshot_digest
        );
        assert_ne!(
            reordered.instruction_identity_digest,
            original.instruction_identity_digest
        );
    }

    fn load_identity(json: &str) -> DelegationIdentity {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn frozen_delegation_identities_keep_nested_bindings() {
        for identity in [
            load_identity(include_str!(
                "../docs/protocol/examples/v014-identity-release-external-idle.valid.json"
            )),
            load_identity(include_str!(
                "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
            )),
            load_identity(include_str!(
                "../docs/protocol/examples/v014-identity-release-managed-idle.valid.json"
            )),
            load_identity(include_str!(
                "../docs/protocol/examples/v014-identity-release-managed-released.valid.json"
            )),
        ] {
            identity.validate().unwrap();
        }
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        identity.complete_result.source.scope_ref = "other".to_owned();
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        identity.complete_result.source.worker_ref = "other-member".to_owned();
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        identity.complete_result.source.accepted_task_digest =
            identity.role_snapshot_digest.clone();
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        identity.caller_scope.binding_ref = "binding:other".to_owned();
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        identity.result_digest.replace_range(7..11, "0000");
        identity.disposition_proof.observed_after_result_digest = identity.result_digest.clone();
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        identity
            .disposition_proof
            .release_receipt
            .as_mut()
            .unwrap()
            .member_ref = "member:other".to_owned();
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        identity
            .disposition_proof
            .release_receipt
            .as_mut()
            .unwrap()
            .scope_ref = "engagement:other".to_owned();
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        let membership = identity.disposition_proof.membership_revision;
        identity
            .disposition_proof
            .release_receipt
            .as_mut()
            .unwrap()
            .terminal_membership_revision = membership;
        assert!(identity.validate().is_ok());
        identity
            .disposition_proof
            .release_receipt
            .as_mut()
            .unwrap()
            .terminal_membership_revision = membership + 1;
        assert!(identity.validate().is_err());
    }

    #[test]
    fn delegation_identity_rejects_closed_field_violations() {
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-idle.valid.json"
        ));
        identity.rounds_used = 2;
        assert!(identity.validate().is_err());
        identity.rounds_used = 1;
        identity.caller_scope.max_rounds = 1;
        identity.unresolved_assignments = 1;
        identity.rounds_used = 2;
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-idle.valid.json"
        ));
        identity.role_body_sha256 = format!("sha256:{}", &identity.role_body_sha256[..64 - 7]);
        assert!(identity.validate().is_err());
        let mut identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-idle.valid.json"
        ));
        identity.role_body_byte_length = 65_537;
        assert!(identity.validate().is_err());
        identity.role_body_byte_length = 65_536;
        assert!(identity.validate().is_ok());
        identity.worker_disposition = "released".to_owned();
        assert!(identity.validate().is_err());
        let raw = include_str!(
            "../docs/protocol/examples/v014-identity-release-external-idle.valid.json"
        );
        let mut missing = serde_json::from_str::<serde_json::Value>(raw).unwrap();
        missing.as_object_mut().unwrap().remove("binding_ref");
        assert!(serde_json::from_value::<DelegationIdentity>(missing).is_err());
        let mut unknown = serde_json::from_str::<serde_json::Value>(raw).unwrap();
        unknown["receipt_digest"] =
            json!("sha256:0000000000000000000000000000000000000000000000000000000000000000");
        assert!(serde_json::from_value::<DelegationIdentity>(unknown).is_err());
        let mut float = serde_json::from_str::<serde_json::Value>(raw).unwrap();
        float["rounds_used"] = json!(1.0);
        assert!(serde_json::from_value::<DelegationIdentity>(float).is_err());
        let duplicate = raw.replacen('{', "{\"schema\":\"dolgorae.delegation-identity/v1\",", 1);
        assert!(serde_json::from_str::<DelegationIdentity>(&duplicate).is_err());
    }

    #[test]
    fn release_operation_branches_and_revision_order_use_frozen_records() {
        for json in [
            include_str!(
                "../docs/protocol/examples/v014-state-release-external-requested.valid.json"
            ),
            include_str!(
                "../docs/protocol/examples/v014-state-release-external-settling.valid.json"
            ),
            include_str!(
                "../docs/protocol/examples/v014-state-release-external-terminal.valid.json"
            ),
            include_str!(
                "../docs/protocol/examples/v014-state-release-managed-cleanup_failed.valid.json"
            ),
        ] {
            serde_json::from_str::<MemberReleaseOperation>(json)
                .unwrap()
                .validate()
                .unwrap();
        }
        let mut failed = serde_json::from_str::<MemberReleaseOperation>(include_str!(
            "../docs/protocol/examples/v014-state-release-managed-cleanup_failed.valid.json"
        ))
        .unwrap();
        failed.progress = "recovery_needed".to_owned();
        failed.required_action = "recover_run".to_owned();
        assert!(failed.validate().is_ok());
        failed.required_action = "none".to_owned();
        assert!(failed.validate().is_err());
        assert!(
            serde_json::from_str::<MemberReleaseOperation>(include_str!(
                "../docs/protocol/examples/v014-state-release-managed.invalid-kind-scope.json"
            ))
            .unwrap()
            .validate()
            .is_err()
        );
        assert!(
            serde_json::from_str::<MemberReleaseOperation>(include_str!(
                "../docs/protocol/examples/v014-state-release-external-requested.invalid-completion.json"
            ))
            .unwrap()
            .validate()
            .is_err()
        );
        assert!(
            serde_json::from_str::<MemberReleaseOperation>(include_str!(
                "../docs/protocol/examples/v014-state-release-managed.invalid-negative-admitted.json"
            ))
            .is_err()
        );
        let identity = load_identity(include_str!(
            "../docs/protocol/examples/v014-identity-release-external-released.valid.json"
        ));
        let proof = identity.disposition_proof.release_receipt.clone().unwrap();
        let mut operation = serde_json::from_str::<MemberReleaseOperation>(include_str!(
            "../docs/protocol/examples/v014-state-release-external-terminal.valid.json"
        ))
        .unwrap();
        operation.terminal_membership_revision = Some(proof.terminal_membership_revision);
        operation.settled_workload_revision = Some(proof.settled_workload_revision);
        release_revision_order(&operation, &proof, &identity.disposition_proof).unwrap();
        let mut observed = identity.disposition_proof.clone();
        observed.membership_revision = proof.terminal_membership_revision;
        release_revision_order(&operation, &proof, &observed).unwrap();
        observed.membership_revision = proof.terminal_membership_revision - 1;
        assert!(release_revision_order(&operation, &proof, &observed).is_err());
        operation.admitted_membership_revision = proof.terminal_membership_revision;
        observed.membership_revision = proof.terminal_membership_revision;
        assert!(release_revision_order(&operation, &proof, &observed).is_err());
    }

    fn frozen_vectors() -> Value {
        serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap()
    }

    fn frozen_vector(id: &str) -> Value {
        frozen_vectors()["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|vector| vector["id"] == id)
            .unwrap_or_else(|| panic!("missing vector {id}"))["input"]
            .clone()
    }

    fn capture_store(label: &str) -> (crate::paths::DolgoraeHome, std::path::PathBuf) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("dolgorae-identity-{label}-{sequence}"));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = crate::paths::DolgoraeHome::from_canonical_home(root).unwrap();
        std::fs::create_dir_all(home.root()).unwrap();
        let mut permissions = std::fs::metadata(home.root()).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
        std::fs::set_permissions(home.root(), permissions).unwrap();
        let store = crate::instructions::InstructionSourceStore::new(&home);
        store.initialize_templates().unwrap();
        let root = home.root().to_path_buf();
        (home, root)
    }

    fn selection(role_name: &str) -> crate::instructions::DelegationSourceSelection {
        crate::instructions::DelegationSourceSelection {
            role_name: role_name.to_owned(),
            role: crate::instructions::SelectedInstructionSource::File(vec![
                "roles".to_owned(),
                role_name.to_owned(),
            ]),
            dependencies: vec![crate::instructions::SelectedNamedSource {
                name: "dependency".to_owned(),
                source: crate::instructions::SelectedInstructionSource::Inline(
                    "captured dependency".to_owned(),
                ),
            }],
            selected_guidance: vec![crate::instructions::SelectedNamedSource {
                name: "vector-api".to_owned(),
                source: crate::instructions::SelectedInstructionSource::Inline(
                    "explicit guidance".to_owned(),
                ),
            }],
        }
    }

    #[test]
    fn delegated_capture_composes_the_closed_instructions_v3_identity() {
        let (home, root) = capture_store("compose");
        let store = crate::instructions::InstructionSourceStore::new(&home);
        let capture = store.capture_delegation(&selection("general")).unwrap();
        let identity = InstructionSnapshot::from_capture(capture.clone()).unwrap();
        assert_eq!(identity.schema, "dolgorae.instructions/v3");
        assert_eq!(identity.dependencies.len(), 1);
        assert_eq!(identity.dependencies[0].name, "dependency");
        assert_eq!(identity.selected_guidance[0].name, "vector-api");
        assert_eq!(
            identity.instruction_identity.composition_version,
            "core-common-role-dependencies-guidance-lf2/v1"
        );
        // Recovery reuses accepted bytes and never reloads a source.
        assert_eq!(
            InstructionSnapshot::from_capture(capture).unwrap(),
            identity
        );
        std::fs::remove_dir_all(root.join("roles")).unwrap();
        std::fs::remove_file(root.join("home/AGENTS.md")).unwrap();
        assert_eq!(
            InstructionSnapshot::from_capture(
                store
                    .capture_delegation(&selection("missing"))
                    .unwrap_or_else(|_| capture_for_recovery(&identity))
            )
            .unwrap(),
            identity
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn capture_for_recovery(
        identity: &InstructionSnapshot,
    ) -> crate::instructions::DelegationCapture {
        crate::instructions::DelegationCapture {
            common: identity.common.clone(),
            role: identity.role.clone(),
            dependencies: identity
                .dependencies
                .iter()
                .map(|source| crate::instructions::CapturedNamedSource {
                    name: source.name.clone(),
                    body: source.source.body.clone(),
                })
                .collect(),
            selected_guidance: identity
                .selected_guidance
                .iter()
                .map(|source| crate::instructions::CapturedNamedSource {
                    name: source.name.clone(),
                    body: source.source.body.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn ambient_profile_guidance_never_changes_a_captured_identity() {
        let (home, root) = capture_store("ambient");
        let store = crate::instructions::InstructionSourceStore::new(&home);
        let codex_home = root.join("profile-codex");
        std::fs::create_dir(&codex_home).unwrap();
        let mut permissions = std::fs::metadata(&codex_home).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
        std::fs::set_permissions(&codex_home, permissions).unwrap();
        let ambient = codex_home.join("AGENTS.md");
        let digest = format!("sha256:{}", "c".repeat(64));

        // Compose with no ambient source present at all.
        let capture = store.capture_delegation(&selection("general")).unwrap();
        let before = InstructionSnapshot::from_capture(capture.clone()).unwrap();
        assert_eq!(
            crate::instructions::observe_profile_ambient(&codex_home, &digest)
                .unwrap()
                .observation("AGENTS.md"),
            Some(&crate::instructions::AmbientGuidanceObservation::Absent)
        );

        // ADDING an ambient source changes only the observation.
        std::fs::write(&ambient, "Ambient line\n").unwrap();
        let mut permissions = std::fs::metadata(&ambient).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o644);
        std::fs::set_permissions(&ambient, permissions).unwrap();
        let observed = crate::instructions::observe_profile_ambient(&codex_home, &digest).unwrap();
        assert!(matches!(
            observed.observation("AGENTS.md"),
            Some(crate::instructions::AmbientGuidanceObservation::Present { .. })
        ));
        let added = InstructionSnapshot::from_capture(capture.clone()).unwrap();
        assert_eq!(added, before);

        // The observation never re-enters a captured object.
        let encoded = serde_json::to_string(&before).unwrap();
        assert!(!encoded.contains("Ambient line"));

        // EDITING the ambient source after compose changes nothing captured.
        std::fs::write(&ambient, "Different ambient line\r\n").unwrap();
        let observed_again =
            crate::instructions::observe_profile_ambient(&codex_home, &digest).unwrap();
        assert_ne!(observed, observed_again);
        let after = InstructionSnapshot::from_capture(capture.clone()).unwrap();
        assert_eq!(after.role_snapshot_digest, before.role_snapshot_digest);
        assert_eq!(
            after.instruction_identity_digest,
            before.instruction_identity_digest
        );
        assert_eq!(after, before);

        // REMOVING the ambient source is equally outside every identity.
        std::fs::remove_file(&ambient).unwrap();
        assert_eq!(
            crate::instructions::observe_profile_ambient(&codex_home, &digest)
                .unwrap()
                .observation("AGENTS.md"),
            Some(&crate::instructions::AmbientGuidanceObservation::Absent)
        );
        assert_eq!(
            InstructionSnapshot::from_capture(capture_for_recovery(&before)).unwrap(),
            before
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ambient_disclosure_rejects_duplicate_or_unknown_sources() {
        use crate::instructions::{
            AmbientGuidanceDisclosure, AmbientGuidanceObservation, AmbientGuidanceSource,
        };
        let digest = format!("sha256:{}", "d".repeat(64));
        let absent = AmbientGuidanceSource {
            name: "AGENTS.md".to_owned(),
            observation: AmbientGuidanceObservation::Absent,
        };
        assert!(
            AmbientGuidanceDisclosure::new(&digest, vec![absent.clone(), absent.clone()]).is_err()
        );
        assert!(
            AmbientGuidanceDisclosure::new(
                &digest,
                vec![AmbientGuidanceSource {
                    name: "OTHER.md".to_owned(),
                    observation: AmbientGuidanceObservation::Absent,
                }]
            )
            .is_err()
        );
        assert!(
            AmbientGuidanceDisclosure::new(
                &digest,
                vec![
                    AmbientGuidanceSource {
                        name: "AGENTS.override.md".to_owned(),
                        observation: AmbientGuidanceObservation::Absent,
                    },
                    absent
                ]
            )
            .is_ok()
        );
    }

    #[test]
    fn same_role_name_and_one_dependency_change_the_captured_identities() {
        let (home, root) = capture_store("drift");
        let store = crate::instructions::InstructionSourceStore::new(&home);
        let base = store.capture_delegation(&selection("general")).unwrap();
        let base = InstructionSnapshot::from_capture(base).unwrap();

        // Same Role alias, one body byte changed: both digests move.
        let mut edited = selection("general");
        edited.role =
            crate::instructions::SelectedInstructionSource::Inline(base.role.body.clone() + " ");
        let edited =
            InstructionSnapshot::from_capture(store.capture_delegation(&edited).unwrap()).unwrap();
        assert_eq!(edited.role.name, base.role.name);
        assert_ne!(edited.role_snapshot_digest, base.role_snapshot_digest);
        assert_ne!(
            edited.instruction_identity_digest,
            base.instruction_identity_digest
        );
        assert_ne!(edited.role.body, base.role.body);

        // One dependency's captured bytes changed: the identity moves.
        let mut drift = selection("general");
        drift.dependencies[0].source = crate::instructions::SelectedInstructionSource::Inline(
            "captured dependency ".to_owned(),
        );
        let drift =
            InstructionSnapshot::from_capture(store.capture_delegation(&drift).unwrap()).unwrap();
        // `role-snapshot/v1` covers the Role body plus dependencies and selected
        // guidance, so a dependency byte change moves both digests.
        assert_ne!(drift.role_snapshot_digest, base.role_snapshot_digest);
        assert_ne!(
            drift.instruction_identity_digest,
            base.instruction_identity_digest
        );
        assert_ne!(
            drift.dependencies[0].source.body,
            base.dependencies[0].source.body
        );

        // Reordering selected guidance also moves the identity, not the Role.
        let mut reordered = selection("general");
        reordered.selected_guidance.insert(
            0,
            crate::instructions::SelectedNamedSource {
                name: "earlier".to_owned(),
                source: crate::instructions::SelectedInstructionSource::Inline("first".to_owned()),
            },
        );
        let reordered =
            InstructionSnapshot::from_capture(store.capture_delegation(&reordered).unwrap())
                .unwrap();
        assert_ne!(reordered.role_snapshot_digest, base.role_snapshot_digest);
        assert_ne!(
            reordered.instruction_identity_digest,
            base.instruction_identity_digest
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn from_capture_refuses_duplicate_aliases_and_the_aggregate_overflow() {
        let (home, root) = capture_store("refuse");
        let store = crate::instructions::InstructionSourceStore::new(&home);
        let mut duplicate = selection("general");
        duplicate.dependencies = vec![
            crate::instructions::SelectedNamedSource {
                name: "same".to_owned(),
                source: crate::instructions::SelectedInstructionSource::Inline("1".to_owned()),
            },
            crate::instructions::SelectedNamedSource {
                name: "same".to_owned(),
                source: crate::instructions::SelectedInstructionSource::Inline("2".to_owned()),
            },
        ];
        let capture = store.capture_delegation(&duplicate).unwrap();
        assert!(InstructionSnapshot::from_capture(capture).is_err());
        // The aggregate ceiling spans common, Role, dependencies and guidance.
        let mut oversized = selection("general");
        oversized.dependencies = vec![crate::instructions::SelectedNamedSource {
            name: "big".to_owned(),
            source: crate::instructions::SelectedInstructionSource::Inline(
                "x".repeat(crate::instructions::MAX_SOURCE_BYTES),
            ),
        }];
        let capture = store.capture_delegation(&oversized).unwrap();
        assert!(InstructionSnapshot::from_capture(capture).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn receipt_namespace_requires_a_paired_scope() {
        // The frozen facade example carries a scoped receipt; the same shape with
        // a null scope is the pre-allocation form (MDC:385).
        let suite: Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/successor-facade-v5-methods.valid.json"
        ))
        .unwrap();
        let mut receipt: OperationReceipt = serde_json::from_value(
            suite["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["operation"] == "install_provisioning")
                .unwrap()["response"]["receipt"]
                .clone(),
        )
        .unwrap();
        receipt.validate().unwrap();

        // A scope names both fields or neither.
        let mut half = receipt.clone();
        half.namespace.scope_ref = None;
        assert!(half.validate().is_err());
        let mut half = receipt.clone();
        half.namespace.scope_kind = None;
        assert!(half.validate().is_err());
        let mut unscoped = receipt.clone();
        unscoped.namespace.scope_kind = None;
        unscoped.namespace.scope_ref = None;
        unscoped.validate().unwrap();
        // A pre-allocation operation may carry the null scope.
        receipt.namespace.scope_kind = None;
        receipt.namespace.scope_ref = None;
        receipt.validate().unwrap();
    }

    #[test]
    fn frozen_vector_inventory_is_the_tasks_exact_subset() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        const ENVELOPE: &str = "dolgorae.mutation-envelope/v1";
        let owned = [
            ROLE_SCHEMA,
            INSTRUCTION_SCHEMA,
            CONFIGURATION_SCHEMA,
            ACCEPTED_TASK_SCHEMA,
            COMPLETE_RESULT_SCHEMA,
            ENVELOPE,
        ];
        let deferred = [
            "dolgorae.group-assignment/v1",
            "dolgorae.merged-findings/v1",
        ];
        let positive = vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|vector| owned.contains(&vector["domain"].as_str().unwrap()))
            .count();
        let negative = vectors["invalid"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|vector| owned.contains(&vector["domain"].as_str().unwrap()))
            .count();
        let deferred_vectors = vectors["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|vector| deferred.contains(&vector["domain"].as_str().unwrap()))
            .count();
        assert_eq!(positive, 25);
        assert_eq!(negative, 21);
        assert_eq!(deferred_vectors, 2);
        assert_eq!(vectors["vectors"].as_array().unwrap().len(), 27);
        assert_eq!(vectors["invalid"].as_array().unwrap().len(), 23);
        assert_eq!(vectors["relations"].as_array().unwrap().len(), 11);
    }

    #[test]
    fn frozen_relations_keep_distinct_digests() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-v014-digest-vectors-v1.json"
        ))
        .unwrap();
        let digest = |vector: &Value| -> String {
            match vector["domain"].as_str().unwrap() {
                ROLE_SCHEMA => serde_json::from_value::<RoleSnapshot>(vector["input"].clone())
                    .unwrap()
                    .digest(),
                INSTRUCTION_SCHEMA => {
                    serde_json::from_value::<InstructionIdentity>(vector["input"].clone())
                        .unwrap()
                        .digest()
                }
                CONFIGURATION_SCHEMA => {
                    serde_json::from_value::<AdmittedConfiguration>(vector["input"].clone())
                        .unwrap()
                        .digest()
                }
                ACCEPTED_TASK_SCHEMA => {
                    serde_json::from_value::<AcceptedTask>(vector["input"].clone())
                        .unwrap()
                        .digest()
                }
                COMPLETE_RESULT_SCHEMA => {
                    serde_json::from_value::<CompleteResult>(vector["input"].clone())
                        .unwrap()
                        .digest()
                }
                other => panic!("unowned vector domain {other}"),
            }
            .unwrap()
        };
        let by_id = |id: &str| {
            vectors["vectors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|vector| vector["id"] == id)
                .unwrap_or_else(|| panic!("missing vector {id}"))
        };
        // The four mutation-envelope relations are owned by the envelope
        // producer and covered in `operation::tests`.
        const ENVELOPE_RELATIONS: [(&str, &str); 4] = [
            ("mutation-base", "mutation-non-mcp-null"),
            (
                "mutation-resolve-ordinary",
                "mutation-resolve-changed-question",
            ),
            (
                "mutation-resolve-ordinary",
                "mutation-resolve-changed-label",
            ),
            ("mutation-resolve-ordinary", "mutation-resolve-changed-text"),
        ];
        let mut checked = 0;
        for relation in vectors["relations"].as_array().unwrap() {
            let kind = relation["kind"].as_str().unwrap_or("different");
            assert_eq!(kind, "different");
            let left_id = relation["left"].as_str().unwrap();
            let right_id = relation["right"].as_str().unwrap();
            if ENVELOPE_RELATIONS.contains(&(left_id, right_id)) {
                continue;
            }
            let left = digest(by_id(left_id));
            let right = digest(by_id(right_id));
            assert_ne!(left, right, "relation {left_id}");
            checked += 1;
        }
        assert_eq!(checked, 7);
    }

    #[test]
    fn alias_pattern_rejects_only_c0_controls_and_del() {
        let base = frozen_vector("role-base");
        let alias = |name: &str| {
            let mut candidate = base.clone();
            candidate["dependencies"] = serde_json::json!([{
                "name": name,
                "content": {
                    "byte_length": 1,
                    "sha256": format!("sha256:{}", crate::jcs::sha256_hex(b"a")),
                },
            }]);
            serde_json::from_value::<RoleSnapshot>(candidate)
                .map_err(|_| invalid())
                .and_then(|snapshot| snapshot.digest())
        };
        // C1 controls (U+0080-U+009F) are permitted by $defs/alias.
        assert!(alias("role\u{85}alias").is_ok());
        assert!(alias("role\u{9f}alias").is_ok());
        // C0 controls, DEL, slash and backslash are not.
        for refused in [
            "role\u{1}alias",
            "role\u{1f}alias",
            "role\u{7f}alias",
            "a/b",
            "a\\b",
            "",
        ] {
            assert!(alias(refused).is_err(), "accepted alias {refused:?}");
        }
    }

    #[test]
    fn pattern_free_configuration_fields_are_byte_bounded_only() {
        let mut value = frozen_vector("configuration-base");
        // No schema pattern exists for these fields, so control characters are
        // admitted and only the byte ceilings apply.
        value["model"] = serde_json::json!("model\u{1}name");
        value["effort"] = serde_json::json!("low\u{7f}");
        value["profile_binding_schema"] = serde_json::json!("schema\u{9f}x");
        let admitted: AdmittedConfiguration = serde_json::from_value(value.clone()).unwrap();
        admitted.digest().unwrap();
        value["effort"] = serde_json::json!("e".repeat(129));
        let admitted: AdmittedConfiguration = serde_json::from_value(value).unwrap();
        assert!(admitted.digest().is_err());
    }

    #[test]
    fn role_snapshot_bounds_only_its_own_sources() {
        let mut snapshot: RoleSnapshot =
            serde_json::from_value(frozen_vector("role-base")).unwrap();
        // A Role body plus named sources may exceed the instruction aggregate
        // here; that ceiling belongs to instruction-identity/v1.
        let half = (MAX_SOURCE_BYTES / 2) as u64;
        snapshot.dependencies = (0..3)
            .map(|index| NamedSource {
                name: format!("dependency-{index}"),
                content: BytesRef {
                    byte_length: half,
                    sha256: format!("sha256:{}", "a".repeat(64)),
                },
            })
            .collect();
        snapshot.digest().unwrap();
    }
}
