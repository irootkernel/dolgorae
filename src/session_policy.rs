//! Versioned accepted policy and Role inputs to the broker lifecycle.

use crate::machine::MachineError;
use crate::markdown_policy::{InstalledRole, PreparedPolicySession};
use crate::run::AgentConfigurationSnapshot;
use crate::specialist_policy::{InstalledSpecialistPolicy, InstalledSpecialistRole};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionPolicySnapshot {
    Legacy(InstalledSpecialistPolicy),
    Caller(PreparedPolicySession),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerSessionRoleSnapshot {
    pub schema_version: u32,
    pub role: InstalledRole,
    pub agent_configuration: AgentConfigurationSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionRoleSnapshot {
    Legacy(Box<InstalledSpecialistRole>),
    Caller(Box<CallerSessionRoleSnapshot>),
}

impl SessionPolicySnapshot {
    pub(crate) fn validate(&self, state_root: &Path) -> Result<(), MachineError> {
        match self {
            Self::Legacy(policy) => crate::specialist_policy::validate_installed_policy(policy),
            Self::Caller(captures) => captures.validate_for_state_root(state_root),
        }
    }
    pub(crate) fn validate_live(&self, state_root: &Path) -> Result<(), MachineError> {
        self.validate(state_root)?;
        match self {
            Self::Legacy(policy) => crate::specialist_policy::validate_live_provider_policy(policy),
            Self::Caller(captures) => {
                if captures.policy().roles.iter().any(|role| {
                    role.reuse_policy == "reuse_any_compatible"
                        || role.collaboration_source
                        || role.collaboration_target
                        || role.activation_policy == "on_mail"
                }) {
                    return Err(MachineError::new(
                        "POLICY_REJECTED",
                        "policy is outside the live provider slice",
                        false,
                        serde_json::json!({"policy":captures.policy().policy_name,"operation":"run.start"}),
                    ));
                }
                Ok(())
            }
        }
    }
    pub fn digest(&self) -> Result<String, MachineError> {
        match self {
            Self::Legacy(policy) => policy.digest(),
            Self::Caller(captures) => captures.policy().digest(),
        }
    }
    pub fn role(&self, role_ref: &str) -> Result<SessionRoleSnapshot, MachineError> {
        match self {
            Self::Legacy(policy) => policy
                .role(role_ref)
                .cloned()
                .map(|role| SessionRoleSnapshot::Legacy(Box::new(role))),
            Self::Caller(captures) => {
                let configuration = captures.child(role_ref)?.clone().try_into()?;
                let role = captures
                    .policy()
                    .roles
                    .iter()
                    .find(|role| role.role_ref == role_ref)
                    .ok_or_else(|| {
                        MachineError::config_invalid(
                            "policy",
                            "accepted policy has no matching Role",
                        )
                    })?
                    .clone();
                Ok(SessionRoleSnapshot::Caller(Box::new(
                    CallerSessionRoleSnapshot {
                        schema_version: 3,
                        role,
                        agent_configuration: configuration,
                    },
                )))
            }
        }
    }
    pub fn policy_name(&self) -> &str {
        match self {
            Self::Legacy(policy) => &policy.policy_name,
            Self::Caller(captures) => &captures.policy().policy_name,
        }
    }
    pub fn approval_policy(&self) -> &str {
        match self {
            Self::Legacy(policy) => &policy.approval_policy,
            Self::Caller(captures) => &captures.policy().approval_policy,
        }
    }
    pub fn revision(&self) -> u64 {
        match self {
            Self::Legacy(policy) => policy.revision,
            Self::Caller(captures) => captures.policy().revision,
        }
    }
    pub fn max_active_specialists(&self) -> u32 {
        match self {
            Self::Legacy(policy) => policy.max_active_specialists,
            Self::Caller(captures) => captures.policy().max_active_specialists,
        }
    }
}

impl SessionRoleSnapshot {
    pub fn role_ref(&self) -> &str {
        match self {
            Self::Legacy(role) => &role.role_ref,
            Self::Caller(capture) => &capture.role.role_ref,
        }
    }
    pub fn max_active_instances(&self) -> u32 {
        match self {
            Self::Legacy(role) => role.max_active_instances,
            Self::Caller(capture) => capture.role.max_active_instances,
        }
    }
    pub fn reuse_policy(&self) -> &str {
        match self {
            Self::Legacy(role) => &role.reuse_policy,
            Self::Caller(capture) => &capture.role.reuse_policy,
        }
    }
    pub fn allowed_access(&self) -> &[String] {
        match self {
            Self::Legacy(role) => &role.allowed_access,
            Self::Caller(capture) => &capture.role.allowed_access,
        }
    }
    pub fn activation_policy(&self) -> &str {
        match self {
            Self::Legacy(role) => &role.activation_policy,
            Self::Caller(capture) => &capture.role.activation_policy,
        }
    }
    pub fn primary_may_request(&self) -> bool {
        match self {
            Self::Legacy(role) => role.primary_may_request,
            Self::Caller(capture) => capture.role.primary_may_request,
        }
    }
    pub fn collaboration_source(&self) -> bool {
        match self {
            Self::Legacy(role) => role.collaboration_source,
            Self::Caller(capture) => capture.role.collaboration_source,
        }
    }
    pub fn collaboration_target(&self) -> bool {
        match self {
            Self::Legacy(role) => role.collaboration_target,
            Self::Caller(capture) => capture.role.collaboration_target,
        }
    }
    pub fn auto_approve_when_fully_delegated(&self) -> bool {
        match self {
            Self::Legacy(role) => role.auto_approve_when_fully_delegated,
            Self::Caller(capture) => capture.role.auto_approve_when_fully_delegated,
        }
    }
    pub fn agent_configuration(&self) -> &AgentConfigurationSnapshot {
        match self {
            Self::Legacy(role) => &role.agent_configuration,
            Self::Caller(capture) => &capture.agent_configuration,
        }
    }
}
