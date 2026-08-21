use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }
    };
}

string_enum!(RunLifecycle {
    Starting => "starting",
    Idle => "idle",
    Running => "running",
    WaitingInteraction => "waiting_interaction",
    ReconciliationRequired => "reconciliation_required",
    Paused => "paused",
    Closed => "closed",
    StartFailed => "start_failed",
    OutcomeUnknown => "outcome_unknown"
});

string_enum!(AggregateKind {
    OrchestratedSession => "orchestrated_session",
    ExternalSpecialistEngagement => "external_specialist_engagement"
});

string_enum!(ControlMode {
    DirectInteractive => "direct_interactive",
    ManagedAgent => "managed_agent"
});

string_enum!(PurposeKind {
    Interactive => "interactive",
    Planning => "planning",
    Implementation => "implementation",
    Review => "review",
    Research => "research",
    Discussion => "discussion",
    WorkflowStage => "workflow_stage",
    Other => "other"
});

string_enum!(ExecutionLane {
    SharedReadonly => "shared_readonly",
    Dedicated => "dedicated"
});

string_enum!(Assurance {
    BestEffortPersonalAlpha => "best_effort_personal_alpha",
    VerifiedThreadScopedControl => "verified_thread_scoped_control",
    StrongProcessContainment => "strong_process_containment"
});

string_enum!(Access {
    Read => "read",
    Write => "write",
    Transitioning => "transitioning",
    Unsupported => "unsupported",
    Unknown => "unknown"
});

string_enum!(PolicyVerification {
    Verified => "verified",
    Unverified => "unverified",
    Failed => "failed"
});

string_enum!(ControllerKind {
    HumanCli => "human_cli",
    InteractiveClient => "interactive_client",
    WorkflowOrchestrator => "workflow_orchestrator",
    Automation => "automation",
    Other => "other"
});

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Purpose {
    pub kind: PurposeKind,
    pub external_label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControllerIdentity {
    pub controller_id: Uuid,
    pub kind: ControllerKind,
    pub instance_id: String,
    pub subject_id: Option<String>,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyEpoch(pub u64);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    pub access: Access,
    pub verification: PolicyVerification,
    pub policy_epoch: PolicyEpoch,
    pub thread_generation: Option<u64>,
    pub server_epoch: Option<u64>,
    pub writer_generation: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_use_checked_wire_names() {
        assert_eq!(ControlMode::ManagedAgent.as_str(), "managed_agent");
        assert_eq!(ExecutionLane::SharedReadonly.as_str(), "shared_readonly");
        assert_eq!(RunLifecycle::StartFailed.as_str(), "start_failed");
        assert_eq!(serde_json::to_string(&Access::Write).unwrap(), "\"write\"");
    }
}
