//! Strictly driver-only Service Provider Interface (SPI) for Dolgorae v2.
//!
//! This crate defines the provider-neutral contract implemented by runtime
//! drivers. Orchestration concerns such as cursors, revisions, event-stream
//! framing, optimistic concurrency conflicts, and inline/artifact size bounds
//! live in `dolgorae-core` and `dolgorae-api`.
//!
//! ## Tolerant Reader Deserialization Policy
//!
//! All SPI structs ignore unrecognized JSON fields. Tagged SPI enums
//! (`InteractionKind`, `NormalizedEvent`) follow a **strict on known tags,
//! tolerant on unknown tags** policy:
//! - An unrecognized discriminator tag deserializes cleanly into
//!   `InteractionKind::Unsupported` or `NormalizedEvent::Unknown` without error.
//! - A recognized discriminator tag with missing or malformed required fields
//!   returns a deserialization error rather than silently dropping data.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

/// Workspace filesystem access policy requested for a driver session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    ReadOnly,
    WorkspaceWrite,
}

/// Interactive approval behavior requested from a driver session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Interactive,
    AutoDeny,
    AutoApproveScoped,
}

/// Stdio MCP server definition injected into a driver session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInjection {
    pub name: String,
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Configuration for starting a new provider session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSpec {
    pub workspace: PathBuf,
    #[serde(default)]
    pub role_instructions: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    pub access: AccessMode,
    pub approval_policy: ApprovalPolicy,
    #[serde(default)]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerInjection>,
}

/// Configuration for resuming an existing provider session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumeSpec {
    pub provider_session_id: String,
    pub workspace: PathBuf,
    #[serde(default)]
    pub role_instructions: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    pub access: AccessMode,
    pub approval_policy: ApprovalPolicy,
    #[serde(default)]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerInjection>,
}

/// File attachment referenced by a turn input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub path: PathBuf,
    #[serde(default)]
    pub media_type: Option<String>,
}

/// Input submitted for a single turn on a driver session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnInput {
    pub text: String,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default)]
    pub output_schema_override: Option<Value>,
}

/// Driver acknowledgement for an accepted turn submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAck {
    pub turn_id: String,
}

/// Model advertised by a runtime driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub efforts: Vec<String>,
}

/// Runtime availability and feature flags reported by a driver probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverCapabilities {
    pub driver_id: String,
    #[serde(default)]
    pub cli_version: Option<String>,
    pub available: bool,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelDescriptor>,
    pub supports_effort: bool,
    pub supports_interactive_approval: bool,
    pub native_output_schema: bool,
    pub supports_mcp_injection: bool,
    pub supports_resume: bool,
}

/// Semantic category of an interactive decision option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    AllowOnce,
    AllowSession,
    DenyOnce,
    DenySession,
    Cancel,
    Submit,
    #[serde(other)]
    Unknown,
}

/// Opaque interactive decision option surfaced to callers.
///
/// Drivers own any mapping from `decision_id` to native wire values and must
/// never expose native permission grant objects across the SPI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub decision_id: String,
    pub label: String,
    pub kind: DecisionKind,
}

/// Single proposed file mutation surfaced inside a `FileChangeApproval`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeEntry {
    pub path: String,
    #[serde(default)]
    pub move_path: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub unified_diff: Option<String>,
}

/// Choice offered for a structured user question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Single structured question inside a `UserQuestion` interaction.
///
/// `id` is the key callers echo back in `InteractionAnswer.answers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestionItem {
    pub id: String,
    #[serde(default)]
    pub header: Option<String>,
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub allows_other: bool,
    #[serde(default)]
    pub is_secret: bool,
}

/// Provider-neutral classification and payload of an interactive driver request.
///
/// Deserialization is **strict on known `kind` tags, tolerant on unknown tags**:
/// known variants validate their required payload fields, while unrecognized
/// tags deserialize cleanly into `Unsupported` preserving `original_kind` and
/// raw JSON `payload`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionKind {
    CommandApproval {
        command: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    FileChangeApproval {
        changes: Vec<FileChangeEntry>,
        #[serde(default)]
        reason: Option<String>,
    },
    PermissionRequest {
        permissions: Vec<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    UserQuestion {
        questions: Vec<UserQuestionItem>,
    },
    McpElicitation {
        server_name: String,
        message: String,
        #[serde(default)]
        requested_schema: Option<Value>,
    },
    Unsupported {
        original_kind: String,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        payload: Value,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum KnownInteractionKind {
    CommandApproval {
        command: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    FileChangeApproval {
        changes: Vec<FileChangeEntry>,
        #[serde(default)]
        reason: Option<String>,
    },
    PermissionRequest {
        permissions: Vec<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    UserQuestion {
        questions: Vec<UserQuestionItem>,
    },
    McpElicitation {
        server_name: String,
        message: String,
        #[serde(default)]
        requested_schema: Option<Value>,
    },
    Unsupported {
        original_kind: String,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        payload: Value,
    },
}

impl From<KnownInteractionKind> for InteractionKind {
    fn from(known: KnownInteractionKind) -> Self {
        match known {
            KnownInteractionKind::CommandApproval {
                command,
                cwd,
                reason,
            } => Self::CommandApproval {
                command,
                cwd,
                reason,
            },
            KnownInteractionKind::FileChangeApproval { changes, reason } => {
                Self::FileChangeApproval { changes, reason }
            }
            KnownInteractionKind::PermissionRequest {
                permissions,
                reason,
            } => Self::PermissionRequest {
                permissions,
                reason,
            },
            KnownInteractionKind::UserQuestion { questions } => Self::UserQuestion { questions },
            KnownInteractionKind::McpElicitation {
                server_name,
                message,
                requested_schema,
            } => Self::McpElicitation {
                server_name,
                message,
                requested_schema,
            },
            KnownInteractionKind::Unsupported {
                original_kind,
                reason,
                payload,
            } => Self::Unsupported {
                original_kind,
                reason,
                payload,
            },
        }
    }
}

impl<'de> Deserialize<'de> for InteractionKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let tag = value
            .as_object()
            .and_then(|map| map.get("kind"))
            .and_then(Value::as_str);

        let is_known_tag = matches!(
            tag,
            Some(
                "command_approval"
                    | "file_change_approval"
                    | "permission_request"
                    | "user_question"
                    | "mcp_elicitation"
                    | "unsupported"
            )
        );

        if is_known_tag {
            serde_json::from_value::<KnownInteractionKind>(value)
                .map(Self::from)
                .map_err(serde::de::Error::custom)
        } else {
            let original_kind = tag
                .map(str::to_owned)
                .or_else(|| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_owned());
            Ok(Self::Unsupported {
                original_kind,
                reason: Some("unrecognized interaction kind".to_owned()),
                payload: value,
            })
        }
    }
}

/// Interactive request emitted by a driver during an active turn.
///
/// All provider-native request and decision identifiers are normalized to
/// opaque strings by the driver. `max_response_bytes == None` indicates that
/// the runtime's default inline response ceiling (256 KiB) applies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionRequest {
    pub interaction_id: String,
    pub turn_id: String,
    pub kind: InteractionKind,
    pub original_kind: String,
    pub title: String,
    #[serde(default)]
    pub decisions: Vec<DecisionOption>,
    #[serde(default)]
    pub protected: bool,
    #[serde(default)]
    pub response_schema: Option<Value>,
    #[serde(default)]
    pub max_response_bytes: Option<usize>,
}

/// Caller answer routed back to a driver for a pending interaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswer {
    pub interaction_id: String,
    pub decision_id: String,
    #[serde(default)]
    pub answers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub payload: Option<Value>,
}

/// Lifecycle state of a tool activity notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActivityStatus {
    Started,
    Delta,
    Completed,
    Failed,
    #[serde(other)]
    Unknown,
}

/// Token and cost accounting reported by a driver.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageMetrics {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cached_input_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
}

/// Terminal outcome of a completed turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnTerminalStatus {
    Completed,
    Interrupted,
    Failed,
    #[serde(other)]
    Unknown,
}

/// Normalized provider-neutral event emitted by a `DriverSession`.
///
/// Deserialization is **strict on known `type` tags, tolerant on unknown tags**:
/// known event types validate their fields strictly, while unrecognized `type`
/// values fall back to `NormalizedEvent::Unknown`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NormalizedEvent {
    TurnStarted {
        turn_id: String,
    },
    TextDelta {
        turn_id: String,
        #[serde(default)]
        item_id: Option<String>,
        delta: String,
    },
    AssistantMessage {
        turn_id: String,
        #[serde(default)]
        item_id: Option<String>,
        text: String,
        #[serde(default)]
        structured_output: Option<Value>,
    },
    ToolActivity {
        turn_id: String,
        item_id: String,
        tool_name: String,
        status: ToolActivityStatus,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        exit_status: Option<i32>,
        #[serde(default)]
        paths: Vec<String>,
    },
    InteractionRequested {
        request: InteractionRequest,
    },
    InteractionResolved {
        interaction_id: String,
        turn_id: String,
        #[serde(default)]
        decision_id: Option<String>,
    },
    Usage {
        #[serde(default)]
        turn_id: Option<String>,
        metrics: UsageMetrics,
    },
    TurnCompleted {
        turn_id: String,
        status: TurnTerminalStatus,
        #[serde(default)]
        error: Option<String>,
    },
    Error {
        #[serde(default)]
        turn_id: Option<String>,
        message: String,
        #[serde(default)]
        will_retry: bool,
    },
    SessionClosed {
        #[serde(default)]
        reason: Option<String>,
    },
    Unknown {
        event_type: String,
        #[serde(default)]
        payload: Option<Value>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KnownNormalizedEvent {
    TurnStarted {
        turn_id: String,
    },
    TextDelta {
        turn_id: String,
        #[serde(default)]
        item_id: Option<String>,
        delta: String,
    },
    AssistantMessage {
        turn_id: String,
        #[serde(default)]
        item_id: Option<String>,
        text: String,
        #[serde(default)]
        structured_output: Option<Value>,
    },
    ToolActivity {
        turn_id: String,
        item_id: String,
        tool_name: String,
        status: ToolActivityStatus,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        exit_status: Option<i32>,
        #[serde(default)]
        paths: Vec<String>,
    },
    InteractionRequested {
        request: InteractionRequest,
    },
    InteractionResolved {
        interaction_id: String,
        turn_id: String,
        #[serde(default)]
        decision_id: Option<String>,
    },
    Usage {
        #[serde(default)]
        turn_id: Option<String>,
        metrics: UsageMetrics,
    },
    TurnCompleted {
        turn_id: String,
        status: TurnTerminalStatus,
        #[serde(default)]
        error: Option<String>,
    },
    Error {
        #[serde(default)]
        turn_id: Option<String>,
        message: String,
        #[serde(default)]
        will_retry: bool,
    },
    SessionClosed {
        #[serde(default)]
        reason: Option<String>,
    },
    Unknown {
        event_type: String,
        #[serde(default)]
        payload: Option<Value>,
    },
}

impl From<KnownNormalizedEvent> for NormalizedEvent {
    fn from(known: KnownNormalizedEvent) -> Self {
        match known {
            KnownNormalizedEvent::TurnStarted { turn_id } => Self::TurnStarted { turn_id },
            KnownNormalizedEvent::TextDelta {
                turn_id,
                item_id,
                delta,
            } => Self::TextDelta {
                turn_id,
                item_id,
                delta,
            },
            KnownNormalizedEvent::AssistantMessage {
                turn_id,
                item_id,
                text,
                structured_output,
            } => Self::AssistantMessage {
                turn_id,
                item_id,
                text,
                structured_output,
            },
            KnownNormalizedEvent::ToolActivity {
                turn_id,
                item_id,
                tool_name,
                status,
                summary,
                command,
                exit_status,
                paths,
            } => Self::ToolActivity {
                turn_id,
                item_id,
                tool_name,
                status,
                summary,
                command,
                exit_status,
                paths,
            },
            KnownNormalizedEvent::InteractionRequested { request } => {
                Self::InteractionRequested { request }
            }
            KnownNormalizedEvent::InteractionResolved {
                interaction_id,
                turn_id,
                decision_id,
            } => Self::InteractionResolved {
                interaction_id,
                turn_id,
                decision_id,
            },
            KnownNormalizedEvent::Usage { turn_id, metrics } => Self::Usage { turn_id, metrics },
            KnownNormalizedEvent::TurnCompleted {
                turn_id,
                status,
                error,
            } => Self::TurnCompleted {
                turn_id,
                status,
                error,
            },
            KnownNormalizedEvent::Error {
                turn_id,
                message,
                will_retry,
            } => Self::Error {
                turn_id,
                message,
                will_retry,
            },
            KnownNormalizedEvent::SessionClosed { reason } => Self::SessionClosed { reason },
            KnownNormalizedEvent::Unknown {
                event_type,
                payload,
            } => Self::Unknown {
                event_type,
                payload,
            },
        }
    }
}

impl<'de> Deserialize<'de> for NormalizedEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let event_type = value
            .as_object()
            .and_then(|map| map.get("type"))
            .and_then(Value::as_str);

        let is_known_tag = matches!(
            event_type,
            Some(
                "turn_started"
                    | "text_delta"
                    | "assistant_message"
                    | "tool_activity"
                    | "interaction_requested"
                    | "interaction_resolved"
                    | "usage"
                    | "turn_completed"
                    | "error"
                    | "session_closed"
                    | "unknown"
            )
        );

        if is_known_tag {
            serde_json::from_value::<KnownNormalizedEvent>(value)
                .map(Self::from)
                .map_err(serde::de::Error::custom)
        } else {
            Ok(Self::Unknown {
                event_type: event_type.unwrap_or("untagged").to_owned(),
                payload: Some(value),
            })
        }
    }
}

/// Errors produced by runtime drivers.
#[derive(Debug, Error)]
pub enum DriverError {
    #[error("driver unavailable: {0}")]
    Unavailable(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("driver process exited unexpectedly: {0}")]
    ProcessExited(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("operation timed out: {0}")]
    Timeout(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

/// Bounded sender side of a driver's normalized event stream.
pub type EventSender = mpsc::Sender<NormalizedEvent>;

/// Bounded receiver side of a driver's normalized event stream.
pub type EventReceiver = mpsc::Receiver<NormalizedEvent>;

/// Creates a bounded channel for normalized driver events.
pub fn event_channel(capacity: usize) -> (EventSender, EventReceiver) {
    mpsc::channel(capacity)
}

/// Factory and capability probe implemented by each runtime driver crate.
#[async_trait]
pub trait AgentDriver: Send + Sync {
    /// Stable identifier for this driver (for example, `"provider-a"`).
    fn driver_id(&self) -> &'static str;

    /// Probes local CLI availability and advertised runtime capabilities.
    async fn probe(&self) -> Result<DriverCapabilities, DriverError>;

    /// Launches a fresh interactive session on the underlying runtime.
    async fn start_session(&self, spec: SessionSpec)
    -> Result<Box<dyn DriverSession>, DriverError>;

    /// Resumes an existing session by its opaque provider session identifier.
    async fn resume_session(&self, spec: ResumeSpec)
    -> Result<Box<dyn DriverSession>, DriverError>;
}

/// Result of evaluating an interactive driver request against `AccessMode` and `ApprovalPolicy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoDecision {
    /// Automatically approve without prompting a caller.
    Allow,
    /// Automatically deny without prompting a caller.
    Deny { reason: String },
    /// Escalate to the caller for an interactive decision.
    Ask,
}

/// Evaluates an `InteractionKind` against the session's `AccessMode`,
/// `ApprovalPolicy`, workspace root, and whether the runtime enforces an OS
/// sandbox (`os_sandboxed`).
///
/// Policy invariants:
/// - `AccessMode::ReadOnly` is a safety floor: `FileChangeApproval` is always
///   denied regardless of `ApprovalPolicy`.
/// - Automatic policies (`AutoDeny`, `AutoApproveScoped`) never return `Ask`.
/// - `AutoApproveScoped` approves only:
///   - `FileChangeApproval` under `WorkspaceWrite` when every target path (and
///     optional `move_path`) lexically normalizes inside `workspace`, and
///   - `CommandApproval` only when `os_sandboxed == true` and `cwd` (if
///     present) lexically normalizes inside `workspace`.
///   - `PermissionRequest`, `McpElicitation`, `UserQuestion`, and `Unsupported`
///     are always denied.
/// - Path containment is lexical because this helper is pure and does not touch
///   the filesystem; symlink escape remains a residual risk mitigated by OS
///   sandboxing and core-level path canonicalization.
pub fn evaluate_interaction(
    kind: &InteractionKind,
    access: AccessMode,
    policy: ApprovalPolicy,
    workspace: &Path,
    os_sandboxed: bool,
) -> AutoDecision {
    // Safety floor: ReadOnly never permits file mutation under any policy.
    if access == AccessMode::ReadOnly && matches!(kind, InteractionKind::FileChangeApproval { .. })
    {
        return AutoDecision::Deny {
            reason: "workspace access is read_only".to_owned(),
        };
    }

    match policy {
        ApprovalPolicy::AutoDeny => AutoDecision::Deny {
            reason: "approval_policy is auto_deny".to_owned(),
        },
        ApprovalPolicy::Interactive => AutoDecision::Ask,
        ApprovalPolicy::AutoApproveScoped => match kind {
            InteractionKind::FileChangeApproval { changes, .. } => {
                if changes.is_empty() {
                    return AutoDecision::Deny {
                        reason: "file change approval contains no paths".to_owned(),
                    };
                }
                let all_inside = changes.iter().all(|entry| {
                    is_lexically_within(workspace, Path::new(&entry.path))
                        && entry
                            .move_path
                            .as_deref()
                            .is_none_or(|dst| is_lexically_within(workspace, Path::new(dst)))
                });
                if all_inside {
                    AutoDecision::Allow
                } else {
                    AutoDecision::Deny {
                        reason: "file change path escapes workspace".to_owned(),
                    }
                }
            }
            InteractionKind::CommandApproval { cwd, .. } => {
                if !os_sandboxed {
                    return AutoDecision::Deny {
                        reason: "command auto-approval requires an OS sandbox".to_owned(),
                    };
                }
                let cwd_ok = cwd
                    .as_deref()
                    .is_none_or(|dir| is_lexically_within(workspace, Path::new(dir)));
                if cwd_ok {
                    AutoDecision::Allow
                } else {
                    AutoDecision::Deny {
                        reason: "command cwd escapes workspace".to_owned(),
                    }
                }
            }
            InteractionKind::PermissionRequest { .. }
            | InteractionKind::McpElicitation { .. }
            | InteractionKind::UserQuestion { .. }
            | InteractionKind::Unsupported { .. } => AutoDecision::Deny {
                reason: "interaction kind is outside auto_approve_scoped policy".to_owned(),
            },
        },
    }
}

fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut stack: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match stack.last() {
                Some(Component::Normal(_)) => {
                    stack.pop();
                }
                _ => return None,
            },
            other => stack.push(other),
        }
    }
    let mut out = PathBuf::new();
    for comp in stack {
        out.push(comp.as_os_str());
    }
    Some(out)
}

fn is_lexically_within(workspace: &Path, candidate: &Path) -> bool {
    let Some(norm_ws) = normalize_lexical(workspace) else {
        return false;
    };
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        norm_ws.join(candidate)
    };
    let Some(norm_cand) = normalize_lexical(&joined) else {
        return false;
    };
    norm_cand.starts_with(&norm_ws)
}

/// Active driver-managed runtime session.
#[async_trait]
pub trait DriverSession: Send + Sync {
    /// Returns the opaque provider-native session identifier for resume.
    fn provider_session_id(&self) -> &str;

    /// Takes ownership of the normalized event stream for this session.
    ///
    /// Callers MUST drain this receiver concurrently while issuing RPC methods
    /// (`send_turn`, `interrupt_turn`, `answer_interaction`, `close`); because
    /// drivers apply backpressure on the bounded event channel rather than
    /// dropping events, an undrained full channel can stall the driver's stdout
    /// reader and time out concurrent RPC replies.
    fn take_events(&mut self) -> Option<EventReceiver>;

    /// Submits a new user turn to the running session.
    async fn send_turn(&self, input: TurnInput) -> Result<TurnAck, DriverError>;

    /// Requests interruption of an active turn.
    async fn interrupt_turn(&self, turn_id: &str) -> Result<(), DriverError>;

    /// Delivers a caller decision for a pending interaction request.
    async fn answer_interaction(&self, answer: InteractionAnswer) -> Result<(), DriverError>;

    /// Gracefully terminates the underlying driver session.
    async fn close(&self) -> Result<(), DriverError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_and_turn_specs_round_trip() {
        let mut env = BTreeMap::new();
        env.insert("DOLGORAE_ACTOR_TOKEN".to_owned(), "tok-123".to_owned());

        let spec = SessionSpec {
            workspace: PathBuf::from("/tmp/work"),
            role_instructions: Some("Review carefully.".to_owned()),
            model: Some("model-alpha".to_owned()),
            effort: Some("high".to_owned()),
            access: AccessMode::ReadOnly,
            approval_policy: ApprovalPolicy::Interactive,
            output_schema: Some(
                json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}),
            ),
            mcp_servers: vec![McpServerInjection {
                name: "dolgorae".to_owned(),
                command: PathBuf::from("/usr/local/bin/dolgorae"),
                args: vec!["mcp".to_owned(), "--scoped".to_owned()],
                env,
            }],
        };

        let encoded = serde_json::to_string(&spec).expect("serialize SessionSpec");
        let decoded: SessionSpec = serde_json::from_str(&encoded).expect("deserialize SessionSpec");
        assert_eq!(decoded, spec);

        let resume = ResumeSpec {
            provider_session_id: "sess-opaque-42".to_owned(),
            workspace: spec.workspace.clone(),
            role_instructions: spec.role_instructions.clone(),
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            access: AccessMode::WorkspaceWrite,
            approval_policy: ApprovalPolicy::AutoApproveScoped,
            output_schema: None,
            mcp_servers: spec.mcp_servers.clone(),
        };
        let encoded_resume = serde_json::to_string(&resume).expect("serialize ResumeSpec");
        let decoded_resume: ResumeSpec =
            serde_json::from_str(&encoded_resume).expect("deserialize ResumeSpec");
        assert_eq!(decoded_resume, resume);

        let turn = TurnInput {
            text: "Inspect changes".to_owned(),
            effort: Some("xhigh".to_owned()),
            attachments: vec![AttachmentRef {
                path: PathBuf::from("/tmp/work/diff.patch"),
                media_type: Some("text/x-diff".to_owned()),
            }],
            output_schema_override: Some(json!({"type": "object"})),
        };
        let encoded_turn = serde_json::to_string(&turn).expect("serialize TurnInput");
        let decoded_turn: TurnInput =
            serde_json::from_str(&encoded_turn).expect("deserialize TurnInput");
        assert_eq!(decoded_turn, turn);
    }

    #[test]
    fn all_interaction_kinds_and_request_round_trip() {
        let kinds = vec![
            InteractionKind::CommandApproval {
                command: vec![
                    "cargo".to_owned(),
                    "test".to_owned(),
                    "--workspace".to_owned(),
                ],
                cwd: Some("/tmp/work".to_owned()),
                reason: Some("Run workspace tests".to_owned()),
            },
            InteractionKind::FileChangeApproval {
                changes: vec![FileChangeEntry {
                    path: "src/lib.rs".to_owned(),
                    move_path: Some("src/core.rs".to_owned()),
                    kind: "rename_modify".to_owned(),
                    unified_diff: Some("--- a/src/lib.rs\n+++ b/src/core.rs\n".to_owned()),
                }],
                reason: Some("Refactor module".to_owned()),
            },
            InteractionKind::PermissionRequest {
                permissions: vec!["filesystem:read:/tmp/work".to_owned()],
                reason: Some("Inspect sibling directory".to_owned()),
            },
            InteractionKind::UserQuestion {
                questions: vec![UserQuestionItem {
                    id: "q-deploy-target".to_owned(),
                    header: Some("Target".to_owned()),
                    question: "Which environment?".to_owned(),
                    options: vec![
                        QuestionOption {
                            label: "staging".to_owned(),
                            description: Some("Pre-production cluster".to_owned()),
                        },
                        QuestionOption {
                            label: "prod".to_owned(),
                            description: None,
                        },
                    ],
                    allows_other: true,
                    is_secret: false,
                }],
            },
            InteractionKind::McpElicitation {
                server_name: "vault".to_owned(),
                message: "Confirm token scope".to_owned(),
                requested_schema: Some(
                    json!({"type": "object", "properties": {"scope": {"type": "string"}}}),
                ),
            },
            InteractionKind::Unsupported {
                original_kind: "vendor/customPrompt".to_owned(),
                reason: Some("unmapped vendor prompt".to_owned()),
                payload: json!({"raw": 123}),
            },
        ];

        for (idx, kind) in kinds.into_iter().enumerate() {
            let req = InteractionRequest {
                interaction_id: format!("req-{idx}"),
                turn_id: "turn-7".to_owned(),
                kind: kind.clone(),
                original_kind: format!("native/kind/{idx}"),
                title: format!("Interaction {idx}"),
                decisions: vec![
                    DecisionOption {
                        decision_id: "opt-allow-once".to_owned(),
                        label: "Allow once".to_owned(),
                        kind: DecisionKind::AllowOnce,
                    },
                    DecisionOption {
                        decision_id: "opt-deny".to_owned(),
                        label: "Deny".to_owned(),
                        kind: DecisionKind::DenyOnce,
                    },
                ],
                protected: idx == 3,
                response_schema: if idx == 4 {
                    Some(json!({"type": "object"}))
                } else {
                    None
                },
                max_response_bytes: if idx == 0 { None } else { Some(4096) },
            };

            let encoded = serde_json::to_string(&req).expect("serialize InteractionRequest");
            let decoded: InteractionRequest =
                serde_json::from_str(&encoded).expect("deserialize InteractionRequest");
            assert_eq!(decoded, req);
            assert_eq!(decoded.kind, kind);
        }

        let mut answers = BTreeMap::new();
        answers.insert("q-deploy-target".to_owned(), vec!["staging".to_owned()]);
        let answer = InteractionAnswer {
            interaction_id: "req-3".to_owned(),
            decision_id: "opt-submit".to_owned(),
            answers,
            payload: Some(json!({"note": "approved"})),
        };
        let ans_json = serde_json::to_string(&answer).expect("serialize InteractionAnswer");
        let ans_back: InteractionAnswer =
            serde_json::from_str(&ans_json).expect("deserialize InteractionAnswer");
        assert_eq!(ans_back, answer);
    }

    #[test]
    fn capabilities_and_normalized_events_round_trip() {
        let caps = DriverCapabilities {
            driver_id: "driver-mock".to_owned(),
            cli_version: Some("1.2.3".to_owned()),
            available: true,
            unavailable_reason: None,
            models: vec![ModelDescriptor {
                id: "m-1".to_owned(),
                display_name: "Model One".to_owned(),
                efforts: vec!["low".to_owned(), "high".to_owned()],
            }],
            supports_effort: true,
            supports_interactive_approval: true,
            native_output_schema: false,
            supports_mcp_injection: true,
            supports_resume: true,
        };
        let caps_json = serde_json::to_string(&caps).expect("serialize DriverCapabilities");
        let caps_back: DriverCapabilities =
            serde_json::from_str(&caps_json).expect("deserialize DriverCapabilities");
        assert_eq!(caps_back, caps);

        let events = vec![
            NormalizedEvent::TurnStarted {
                turn_id: "t1".to_owned(),
            },
            NormalizedEvent::TextDelta {
                turn_id: "t1".to_owned(),
                item_id: Some("i1".to_owned()),
                delta: "hello".to_owned(),
            },
            NormalizedEvent::AssistantMessage {
                turn_id: "t1".to_owned(),
                item_id: Some("i1".to_owned()),
                text: "hello world".to_owned(),
                structured_output: Some(json!({"verdict": "ok"})),
            },
            NormalizedEvent::ToolActivity {
                turn_id: "t1".to_owned(),
                item_id: "tool-1".to_owned(),
                tool_name: "shell".to_owned(),
                status: ToolActivityStatus::Completed,
                summary: Some("tests passed".to_owned()),
                command: Some("cargo test".to_owned()),
                exit_status: Some(0),
                paths: vec!["src/lib.rs".to_owned()],
            },
            NormalizedEvent::InteractionResolved {
                interaction_id: "req-1".to_owned(),
                turn_id: "t1".to_owned(),
                decision_id: Some("allow_once".to_owned()),
            },
            NormalizedEvent::Usage {
                turn_id: Some("t1".to_owned()),
                metrics: UsageMetrics {
                    input_tokens: Some(100),
                    output_tokens: Some(50),
                    cached_input_tokens: Some(20),
                    reasoning_tokens: Some(10),
                    total_cost_usd: Some(0.0125),
                },
            },
            NormalizedEvent::TurnCompleted {
                turn_id: "t1".to_owned(),
                status: TurnTerminalStatus::Completed,
                error: None,
            },
            NormalizedEvent::Error {
                turn_id: Some("t1".to_owned()),
                message: "transient upstream timeout".to_owned(),
                will_retry: true,
            },
            NormalizedEvent::SessionClosed {
                reason: Some("process exited cleanly".to_owned()),
            },
            NormalizedEvent::Unknown {
                event_type: "vendor_extension".to_owned(),
                payload: Some(json!({"extra": true})),
            },
        ];

        for event in events {
            let encoded = serde_json::to_string(&event).expect("serialize NormalizedEvent");
            let decoded: NormalizedEvent =
                serde_json::from_str(&encoded).expect("deserialize NormalizedEvent");
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn tolerant_reader_ignores_unknown_fields() {
        let raw_caps = json!({
            "driver_id": "mock",
            "cli_version": "2.0.0",
            "available": true,
            "unavailable_reason": null,
            "models": [
                {
                    "id": "m-next",
                    "display_name": "Next",
                    "efforts": ["high"],
                    "future_model_field": 999
                }
            ],
            "supports_effort": true,
            "supports_interactive_approval": true,
            "native_output_schema": true,
            "supports_mcp_injection": true,
            "supports_resume": true,
            "unrecognized_top_level_field": {"nested": [1, 2, 3]}
        });
        let parsed: DriverCapabilities =
            serde_json::from_value(raw_caps).expect("ignore unknown fields in DriverCapabilities");
        assert_eq!(parsed.driver_id, "mock");
        assert_eq!(parsed.models.len(), 1);

        let raw_event = json!({
            "type": "text_delta",
            "turn_id": "t-9",
            "delta": "chunk",
            "future_metadata": "ignored"
        });
        let event: NormalizedEvent =
            serde_json::from_value(raw_event).expect("ignore unknown fields in NormalizedEvent");
        assert_eq!(
            event,
            NormalizedEvent::TextDelta {
                turn_id: "t-9".to_owned(),
                item_id: None,
                delta: "chunk".to_owned(),
            }
        );
    }

    #[test]
    fn tolerant_reader_maps_unknown_enum_variants_and_rejects_malformed_known_tags() {
        let unknown_decision: DecisionKind =
            serde_json::from_value(json!("biometric_approve_forever"))
                .expect("unknown DecisionKind must deserialize to Unknown");
        assert_eq!(unknown_decision, DecisionKind::Unknown);

        let unknown_status: TurnTerminalStatus =
            serde_json::from_value(json!("rate_limited_paused"))
                .expect("unknown TurnTerminalStatus must deserialize to Unknown");
        assert_eq!(unknown_status, TurnTerminalStatus::Unknown);

        let unknown_tool_status: ToolActivityStatus =
            serde_json::from_value(json!("retrying_upstream"))
                .expect("unknown ToolActivityStatus must deserialize to Unknown");
        assert_eq!(unknown_tool_status, ToolActivityStatus::Unknown);

        let unknown_kind_obj: InteractionKind = serde_json::from_value(json!({
            "kind": "future_sandbox_upgrade",
            "detail": "kernel-cap"
        }))
        .expect("unknown InteractionKind object must deserialize to Unsupported");
        assert!(matches!(
            unknown_kind_obj,
            InteractionKind::Unsupported {
                ref original_kind,
                payload: Value::Object(_),
                ..
            } if original_kind == "future_sandbox_upgrade"
        ));

        let unknown_event: NormalizedEvent = serde_json::from_value(json!({
            "type": "future_provider_heartbeat",
            "turn_id": "t-3",
            "extra": 42
        }))
        .expect("unknown NormalizedEvent must deserialize to Unknown");
        assert!(matches!(
            unknown_event,
            NormalizedEvent::Unknown {
                ref event_type,
                payload: Some(_),
            } if event_type == "future_provider_heartbeat"
        ));

        // Strict on known tags: missing required field `command` on `command_approval` must fail.
        let malformed_known_kind = serde_json::from_value::<InteractionKind>(json!({
            "kind": "command_approval"
        }));
        assert!(
            malformed_known_kind.is_err(),
            "known InteractionKind tag missing required fields must fail"
        );

        let malformed_known_event = serde_json::from_value::<NormalizedEvent>(json!({
            "type": "error",
            "will_retry": false
        }));
        assert!(
            malformed_known_event.is_err(),
            "known NormalizedEvent tag missing required fields must fail"
        );
    }

    #[test]
    fn evaluate_interaction_full_policy_matrix() {
        let ws = Path::new("/work/repo");
        let cmd_in = InteractionKind::CommandApproval {
            command: vec!["cargo".to_owned(), "test".to_owned()],
            cwd: Some("/work/repo/crates".to_owned()),
            reason: None,
        };
        let cmd_out = InteractionKind::CommandApproval {
            command: vec!["ls".to_owned()],
            cwd: Some("/etc".to_owned()),
            reason: None,
        };
        let file_in = InteractionKind::FileChangeApproval {
            changes: vec![FileChangeEntry {
                path: "src/lib.rs".to_owned(),
                move_path: Some("src/core.rs".to_owned()),
                kind: "modify".to_owned(),
                unified_diff: None,
            }],
            reason: None,
        };
        let file_escape = InteractionKind::FileChangeApproval {
            changes: vec![FileChangeEntry {
                path: "../outside.txt".to_owned(),
                move_path: None,
                kind: "modify".to_owned(),
                unified_diff: None,
            }],
            reason: None,
        };
        let file_abs_outside = InteractionKind::FileChangeApproval {
            changes: vec![FileChangeEntry {
                path: "/tmp/evil.sh".to_owned(),
                move_path: None,
                kind: "create".to_owned(),
                unified_diff: None,
            }],
            reason: None,
        };
        let perm = InteractionKind::PermissionRequest {
            permissions: vec!["network:egress".to_owned()],
            reason: None,
        };
        let question = InteractionKind::UserQuestion {
            questions: vec![UserQuestionItem {
                id: "q1".to_owned(),
                header: None,
                question: "Token?".to_owned(),
                options: vec![],
                allows_other: true,
                is_secret: true,
            }],
        };
        let elicit = InteractionKind::McpElicitation {
            server_name: "vault".to_owned(),
            message: "Approve".to_owned(),
            requested_schema: None,
        };
        let unsupported = InteractionKind::Unsupported {
            original_kind: "vendor/x".to_owned(),
            reason: None,
            payload: json!({}),
        };

        let all_kinds = [
            ("cmd_in", &cmd_in),
            ("cmd_out", &cmd_out),
            ("file_in", &file_in),
            ("file_escape", &file_escape),
            ("file_abs_outside", &file_abs_outside),
            ("perm", &perm),
            ("question", &question),
            ("elicit", &elicit),
            ("unsupported", &unsupported),
        ];
        let accesses = [AccessMode::ReadOnly, AccessMode::WorkspaceWrite];
        let policies = [
            ApprovalPolicy::Interactive,
            ApprovalPolicy::AutoDeny,
            ApprovalPolicy::AutoApproveScoped,
        ];
        let sandboxes = [false, true];

        for (label, kind) in all_kinds {
            for access in accesses {
                for policy in policies {
                    for os_sandboxed in sandboxes {
                        let got = evaluate_interaction(kind, access, policy, ws, os_sandboxed);

                        // Invariant: auto policies NEVER return Ask.
                        if matches!(
                            policy,
                            ApprovalPolicy::AutoDeny | ApprovalPolicy::AutoApproveScoped
                        ) {
                            assert!(
                                !matches!(got, AutoDecision::Ask),
                                "auto policy returned Ask for ({label}, {access:?}, {policy:?}, {os_sandboxed})"
                            );
                        }

                        // Invariant: ReadOnly ALWAYS denies FileChangeApproval.
                        if access == AccessMode::ReadOnly
                            && matches!(kind, InteractionKind::FileChangeApproval { .. })
                        {
                            assert!(
                                matches!(got, AutoDecision::Deny { .. }),
                                "ReadOnly must deny FileChangeApproval for ({label}, {policy:?}, {os_sandboxed})"
                            );
                            continue;
                        }

                        match policy {
                            ApprovalPolicy::AutoDeny => {
                                assert!(matches!(got, AutoDecision::Deny { .. }));
                            }
                            ApprovalPolicy::Interactive => {
                                assert_eq!(got, AutoDecision::Ask);
                            }
                            ApprovalPolicy::AutoApproveScoped => {
                                let expect_allow = match label {
                                    "file_in" => access == AccessMode::WorkspaceWrite,
                                    "cmd_in" => os_sandboxed,
                                    _ => false,
                                };
                                if expect_allow {
                                    assert_eq!(
                                        got,
                                        AutoDecision::Allow,
                                        "expected Allow for ({label}, {access:?}, {policy:?}, {os_sandboxed})"
                                    );
                                } else {
                                    assert!(
                                        matches!(got, AutoDecision::Deny { .. }),
                                        "expected Deny for ({label}, {access:?}, {policy:?}, {os_sandboxed})"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
