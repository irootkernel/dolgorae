//! Domain models, strict decimal `Cursor`, and API/MCP contract types.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use dolgorae_spi::{
    AccessMode, ApprovalPolicy, AttachmentRef, DecisionOption, DriverCapabilities, InteractionKind,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::CoreError;
use crate::{MAX_ARTIFACT_BYTES, MAX_INLINE_BYTES, MAX_PAGE_BYTES, MAX_RECEIVE_BYTES};

/// Default maximum delegation nesting depth.
pub const DEFAULT_MAX_DELEGATION_DEPTH: u8 = 3;

/// Hard upper bound for `SessionPolicy.max_delegation_depth`.
pub const HARD_MAX_DELEGATION_DEPTH: u8 = 8;

/// Default maximum durable events retained per session before pruning.
pub const DEFAULT_MAX_RETAINED_EVENTS: u64 = 10_000;

/// Canonical unsigned 64-bit decimal cursor (`^(0|[1-9][0-9]*)$`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Cursor(pub u64);

impl Cursor {
    pub fn parse(raw: &str) -> Result<Self, CoreError> {
        if raw.is_empty() {
            return Err(CoreError::bad_request("cursor must not be empty"));
        }
        let bytes = raw.as_bytes();
        if bytes.len() > 1 && bytes[0] == b'0' {
            return Err(CoreError::bad_request(format!(
                "cursor '{raw}' must not have leading zeros"
            )));
        }
        if !bytes.iter().all(|b| b.is_ascii_digit()) {
            return Err(CoreError::bad_request(format!(
                "cursor '{raw}' must contain only ASCII digits"
            )));
        }
        let val = raw
            .parse::<u64>()
            .map_err(|_| CoreError::bad_request(format!("cursor '{raw}' overflows u64")))?;
        Ok(Self(val))
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for Cursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Cursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Normative size bounds reported by `/v2/capabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeBounds {
    pub max_inline_bytes: usize,
    pub max_artifact_bytes: usize,
    pub max_page_bytes: usize,
    pub max_receive_bytes: usize,
}

impl Default for SizeBounds {
    fn default() -> Self {
        Self {
            max_inline_bytes: MAX_INLINE_BYTES,
            max_artifact_bytes: MAX_ARTIFACT_BYTES,
            max_page_bytes: MAX_PAGE_BYTES,
            max_receive_bytes: MAX_RECEIVE_BYTES,
        }
    }
}

/// Daemon capability probe snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesSnapshot {
    pub api_version: String,
    pub bounds: SizeBounds,
    pub drivers: Vec<DriverCapabilities>,
}

/// Immutable reference to a content-addressed artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub artifact_id: String,
    pub sha256: String,
    pub media_type: String,
    pub byte_len: u64,
}

/// Delegation & collaboration policy inherited by a root session tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPolicy {
    #[serde(default = "default_max_delegation_depth")]
    pub max_delegation_depth: u8,
    #[serde(default = "default_allow_actor_collaboration")]
    pub allow_actor_collaboration: bool,
}

fn default_max_delegation_depth() -> u8 {
    DEFAULT_MAX_DELEGATION_DEPTH
}

fn default_allow_actor_collaboration() -> bool {
    true
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            max_delegation_depth: DEFAULT_MAX_DELEGATION_DEPTH,
            allow_actor_collaboration: true,
        }
    }
}

impl SessionPolicy {
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.max_delegation_depth > HARD_MAX_DELEGATION_DEPTH {
            return Err(CoreError::bad_request(format!(
                "max_delegation_depth {} exceeds hard cap {}",
                self.max_delegation_depth, HARD_MAX_DELEGATION_DEPTH
            )));
        }
        Ok(())
    }
}

/// Origin of a resolved Role / Persona definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleSource {
    Workspace,
    UserGlobal,
}

/// Summary of a Role / Persona definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleSummary {
    pub name: String,
    pub source: RoleSource,
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    pub access: AccessMode,
    pub approval_policy: ApprovalPolicy,
    pub snapshot_sha256: String,
}

/// Full Role / Persona definition including Markdown instructions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleDetail {
    #[serde(flatten)]
    pub summary: RoleSummary,
    pub instructions: String,
    #[serde(default)]
    pub output_schema: Option<Value>,
}

/// Lifecycle state of a durable session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Idle,
    Closed,
    Failed,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }

    pub fn parse_str(s: &str) -> Result<Self, CoreError> {
        match s {
            "active" => Ok(Self::Active),
            "idle" => Ok(Self::Idle),
            "closed" => Ok(Self::Closed),
            "failed" => Ok(Self::Failed),
            other => Err(CoreError::internal(format!(
                "invalid session status '{other}'"
            ))),
        }
    }
}

/// Request to start a root session or delegated child actor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub workspace: PathBuf,
    pub driver: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub access: Option<AccessMode>,
    #[serde(default)]
    pub approval_policy: Option<ApprovalPolicy>,
    #[serde(default)]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub allow_delegation: bool,
    #[serde(default)]
    pub policy: Option<SessionPolicy>,
}

/// Request to resume an idle/detached session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeSessionRequest {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub access: Option<AccessMode>,
    #[serde(default)]
    pub approval_policy: Option<ApprovalPolicy>,
}

/// Coherent session snapshot with monotonic stamp invariants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub root_session_id: String,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    pub depth: u8,
    pub policy: SessionPolicy,
    #[serde(default)]
    pub role: Option<String>,
    pub driver: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    pub workspace: String,
    pub status: SessionStatus,
    pub access: AccessMode,
    pub approval_policy: ApprovalPolicy,
    pub session_generation: u64,
    pub revision: u64,
    pub head_cursor: Cursor,
    pub interaction_revision: u64,
    #[serde(default)]
    pub active_turn_id: Option<String>,
    pub pending_interaction_count: u64,
}

/// Filter for listing sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFilter {
    pub parent_session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub status: Option<SessionStatus>,
}

/// Request to submit a turn to an active session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitTurnRequest {
    pub text: String,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default)]
    pub output_schema_override: Option<Value>,
}

/// Turn acceptance envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAccepted {
    pub session_id: String,
    pub turn_id: String,
    pub revision: u64,
    pub head_cursor: Cursor,
}

/// Durable timeline item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineItem {
    pub cursor: Cursor,
    #[serde(default)]
    pub turn_id: Option<String>,
    pub kind: String,
    pub created_at: String,
    #[serde(default)]
    pub inline_text: Option<String>,
    #[serde(default)]
    pub payload: Option<Value>,
    #[serde(default)]
    pub artifact: Option<ArtifactRef>,
}

/// Bounded timeline page separating `highest_cursor` from opaque `next_page_token`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelinePage {
    pub session_id: String,
    pub revision: u64,
    pub head_cursor: Cursor,
    pub highest_cursor: Cursor,
    #[serde(default)]
    pub next_page_token: Option<String>,
    pub items: Vec<TimelineItem>,
}

/// Lifecycle state of a persisted interaction record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    Pending,
    Resolved,
    Expired,
}

impl InteractionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Expired => "expired",
        }
    }

    pub fn parse_str(s: &str) -> Result<Self, CoreError> {
        match s {
            "pending" => Ok(Self::Pending),
            "resolved" => Ok(Self::Resolved),
            "expired" => Ok(Self::Expired),
            other => Err(CoreError::internal(format!(
                "invalid interaction status '{other}'"
            ))),
        }
    }
}

/// Persisted interaction record with nested `kind` matching OpenAPI `InteractionRecord.kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionRecord {
    pub interaction_id: String,
    pub turn_id: String,
    pub session_generation: u64,
    pub interaction_revision: u64,
    pub session_revision: u64,
    pub kind: InteractionKind,
    pub original_kind: String,
    pub title: String,
    pub decisions: Vec<DecisionOption>,
    pub protected: bool,
    #[serde(default)]
    pub response_schema: Option<Value>,
    #[serde(default)]
    pub max_response_bytes: Option<usize>,
    pub status: InteractionStatus,
}

/// Listing response for session interactions with current revision stamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionList {
    pub session_revision: u64,
    pub interaction_revision: u64,
    pub head_cursor: Cursor,
    pub interactions: Vec<InteractionRecord>,
}

/// Optimistic-concurrency interaction answer request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerInteractionRequest {
    pub turn_id: String,
    pub session_generation: u64,
    pub expected_interaction_revision: u64,
    pub expected_session_revision: u64,
    pub decision_id: String,
    #[serde(default)]
    pub answers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub payload: Option<Value>,
}

/// Resolution acknowledgement for an answered interaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerInteractionResponse {
    pub interaction_id: String,
    pub status: InteractionStatus,
    pub session_revision: u64,
    pub interaction_revision: u64,
    pub head_cursor: Cursor,
}

/// Durable mailbox entry with automatic artifact spill for >256 KiB bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxMessage {
    pub message_id: String,
    pub cursor: Cursor,
    pub session_id: String,
    #[serde(default)]
    pub from_session_id: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub inline_body: Option<String>,
    #[serde(default)]
    pub artifact: Option<ArtifactRef>,
    pub created_at: String,
}

/// Request to deliver a durable mailbox message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostMailboxRequest {
    #[serde(default)]
    pub from_session_id: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    pub body: String,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}

/// Paginated mailbox listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxPage {
    pub highest_cursor: Cursor,
    #[serde(default)]
    pub next_page_token: Option<String>,
    pub messages: Vec<MailboxMessage>,
}

/// Wait join semantics across multiple target sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitMode {
    Any,
    All,
}

/// Request to wait on one or more target sessions with cycle detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitSessionsRequest {
    #[serde(default)]
    pub waiter_session_id: Option<String>,
    #[serde(alias = "target_session_ids")]
    pub session_ids: Vec<String>,
    #[serde(default = "default_wait_mode")]
    pub mode: WaitMode,
    #[serde(default = "default_wait_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_wait_mode() -> WaitMode {
    WaitMode::All
}

fn default_wait_timeout_ms() -> u64 {
    30_000
}

/// Wait completion outcome matching `openapi-v2.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitSessionsResponse {
    pub completed_session_ids: Vec<String>,
    pub timed_out: bool,
    pub sessions: Vec<SessionSnapshot>,
}

/// Single-writer lease on a canonical workspace path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterLease {
    pub workspace: String,
    pub holder_session_id: String,
    pub acquired_at: String,
}

/// Result of core-side JSON Schema validation for non-native drivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaValidationResult {
    pub validated: bool,
    #[serde(default)]
    pub errors: Vec<String>,
}

/// Authenticated scope of a child/delegating actor carrying `DOLGORAE_ACTOR_TOKEN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorScope {
    pub session_id: String,
    pub root_session_id: String,
    pub access: AccessMode,
    pub workspace: PathBuf,
}

/// Caller authorization context passed into every `CoreEngine` method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerContext {
    ExternalHost,
    ScopedActor(ActorScope),
}

/// Event pushed over live session broadcast channels and replayed from SQLite.
///
/// Durable events have `cursor: Some(Cursor)`; ephemeral live-only events
/// (`text_delta`, `tool_delta`, `heartbeat`) and `refresh_required` have `cursor: None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub created_at: String,
    pub payload: Value,
}

/// Result of replaying durable events after a caller's `Last-Event-ID`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReplayResult {
    Events(Vec<SessionEvent>),
    RefreshRequired { scopes: Vec<String> },
}

/// Explicit configuration for `CoreEngine` (never reads `HOME` env var).
#[derive(Debug, Clone)]
pub struct CoreConfig {
    pub home_dir: PathBuf,
    pub max_retained_events_per_session: u64,
    pub event_broadcast_capacity: usize,
    pub mcp_bin_path: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
}

impl CoreConfig {
    pub fn new(home_dir: PathBuf) -> Self {
        Self {
            home_dir,
            max_retained_events_per_session: DEFAULT_MAX_RETAINED_EVENTS,
            event_broadcast_capacity: 256,
            mcp_bin_path: None,
            socket_path: None,
        }
    }
}

/// Formats the current UTC wall-clock timestamp as RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_unix_rfc3339(secs)
}

pub(crate) fn format_unix_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hour = rem / 3_600;
    let min = (rem % 3_600) / 60;
    let sec = rem % 60;

    // Civil date from days since 1970-01-01 (Howard Hinnant algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}
