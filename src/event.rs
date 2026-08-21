use crate::audit::is_microsecond_utc_timestamp;
use crate::jcs::{LosslessJson, canonicalize};
use crate::workspace::LosslessPath;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_JCS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventProjection {
    Minimal,
    Operational,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStateEventPayload {
    pub previous: Option<String>,
    pub current: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnStateEventPayload {
    pub previous: Option<String>,
    pub current: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FinalResponse {
    Inline { text: String },
    Artifact { artifact: Box<ArtifactMetadata> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMetadata {
    pub schema_version: u32,
    pub artifact_id: Uuid,
    pub run_id: Uuid,
    pub kind: String,
    pub visibility: String,
    pub interaction_request_id: Option<Uuid>,
    pub media_type: String,
    pub byte_length: u64,
    pub sha256: String,
    pub created_at: String,
    pub retention: String,
    pub integrity: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEventPayload {
    pub response: FinalResponse,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionOpenedPayload {
    pub request_id: String,
    pub interaction_kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionResolvedPayload {
    pub request_id: String,
    pub outcome: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeErrorPayload {
    pub error_code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsagePayload {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceChangesPayload {
    pub paths: Vec<LosslessPath>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterEventPayload {
    pub previous: String,
    pub current: String,
    pub writer_run_id: Option<Uuid>,
    pub writer_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEventPayload {
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandStartedPayload {
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCompletedPayload {
    pub command: Vec<String>,
    pub exit_status: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticPayload {
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationPayload {
    pub run_generation: u64,
    pub server_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningSuppressionPayload {
    pub method: String,
    pub byte_length: u64,
    pub sha256: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum ClientEventData {
    #[serde(rename = "run.state_changed")]
    RunStateChanged(RunStateEventPayload),
    #[serde(rename = "turn.state_changed")]
    TurnStateChanged(TurnStateEventPayload),
    #[serde(rename = "response.final")]
    ResponseFinal(ResponseEventPayload),
    #[serde(rename = "interaction.opened")]
    InteractionOpened(InteractionOpenedPayload),
    #[serde(rename = "interaction.resolved")]
    InteractionResolved(InteractionResolvedPayload),
    #[serde(rename = "runtime.error")]
    RuntimeError(RuntimeErrorPayload),
    #[serde(rename = "usage.reported")]
    UsageReported(UsagePayload),
    #[serde(rename = "workspace.changes")]
    WorkspaceChanges(WorkspaceChangesPayload),
    #[serde(rename = "writer.state_changed")]
    WriterStateChanged(WriterEventPayload),
    #[serde(rename = "recovery.required")]
    RecoveryRequired(RecoveryEventPayload),
    #[serde(rename = "command.started")]
    CommandStarted(CommandStartedPayload),
    #[serde(rename = "command.completed")]
    CommandCompleted(CommandCompletedPayload),
    #[serde(rename = "diagnostic.reported")]
    DiagnosticReported(DiagnosticPayload),
    #[serde(rename = "generation.changed")]
    GenerationChanged(GenerationPayload),
    #[serde(rename = "reasoning.suppressed")]
    ReasoningSuppressed(ReasoningSuppressionPayload),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientEventRecord {
    pub schema_version: u32,
    pub event_schema_version: u32,
    pub cursor: String,
    pub event_id: Uuid,
    pub timestamp: String,
    pub workspace_id: String,
    pub run_id: Uuid,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub server_key: String,
    pub server_epoch: u64,
    #[serde(flatten)]
    pub data: ClientEventData,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventDelivery {
    pub schema_version: u32,
    pub kind: String,
    pub projection: EventProjection,
    pub replay: bool,
    pub record: ClientEventRecord,
}

#[derive(Debug)]
pub(crate) struct EventError(String);

impl std::fmt::Display for EventError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl ClientEventRecord {
    pub(crate) fn validate(&self, run_id: Uuid, sequence: u64) -> Result<(), EventError> {
        if self.schema_version != 1 || self.event_schema_version != 1 {
            return Err(invalid("unsupported event schema version"));
        }
        if self.cursor != sequence.to_string() {
            return Err(invalid("event cursor must equal the audit sequence"));
        }
        if self.event_id.get_version_num() != 7
            || self.run_id != run_id
            || run_id.get_version_num() != 7
        {
            return Err(invalid("event or run identity is invalid"));
        }
        if !is_microsecond_utc_timestamp(&self.timestamp)
            || !is_sha256(&self.workspace_id)
            || !is_sha256(&self.server_key)
            || self.server_epoch == 0
            || self.server_epoch > MAX_JCS_SAFE_INTEGER
        {
            return Err(invalid("event provenance is invalid"));
        }
        if self
            .thread_id
            .as_deref()
            .is_some_and(|value| !bounded(value, 256))
            || self
                .turn_id
                .as_deref()
                .is_some_and(|value| !bounded(value, 256))
        {
            return Err(invalid("event thread or turn identity is invalid"));
        }
        if self.data.requires_turn() && (self.thread_id.is_none() || self.turn_id.is_none()) {
            return Err(invalid(
                "event type requires exact thread and turn identities",
            ));
        }
        self.data.validate(self.run_id)
    }
}

impl ClientEventData {
    pub(crate) fn minimal(&self) -> bool {
        matches!(
            self,
            Self::RunStateChanged(_)
                | Self::TurnStateChanged(_)
                | Self::ResponseFinal(_)
                | Self::InteractionOpened(_)
                | Self::InteractionResolved(_)
                | Self::RuntimeError(_)
                | Self::WriterStateChanged(_)
                | Self::RecoveryRequired(_)
        )
    }

    fn requires_turn(&self) -> bool {
        matches!(
            self,
            Self::TurnStateChanged(_)
                | Self::ResponseFinal(_)
                | Self::InteractionOpened(_)
                | Self::InteractionResolved(_)
                | Self::UsageReported(_)
                | Self::WorkspaceChanges(_)
                | Self::CommandStarted(_)
                | Self::CommandCompleted(_)
                | Self::ReasoningSuppressed(_)
        )
    }

    fn validate(&self, run_id: Uuid) -> Result<(), EventError> {
        match self {
            Self::RunStateChanged(payload) => {
                if payload
                    .previous
                    .as_deref()
                    .is_some_and(|value| !valid_run_state(value))
                    || !valid_run_state(&payload.current)
                {
                    return Err(invalid("run state event contains an unknown state"));
                }
            }
            Self::TurnStateChanged(payload) => {
                if payload
                    .previous
                    .as_deref()
                    .is_some_and(|value| !valid_turn_state(value))
                    || !valid_turn_state(&payload.current)
                {
                    return Err(invalid("turn state event contains an unknown state"));
                }
            }
            Self::ResponseFinal(payload) => payload.response.validate(run_id)?,
            Self::InteractionOpened(payload) => {
                if !bounded(&payload.request_id, 256)
                    || !matches!(
                        payload.interaction_kind.as_str(),
                        "command_execution_approval"
                            | "file_change_approval"
                            | "user_input"
                            | "unsupported_request"
                    )
                {
                    return Err(invalid("interaction-opened payload is invalid"));
                }
            }
            Self::InteractionResolved(payload) => {
                if !bounded(&payload.request_id, 256)
                    || !matches!(
                        payload.outcome.as_str(),
                        "accepted"
                            | "declined"
                            | "cancelled"
                            | "answered"
                            | "stale"
                            | "method_not_found"
                    )
                {
                    return Err(invalid("interaction-resolved payload is invalid"));
                }
            }
            Self::RuntimeError(payload) => {
                if payload.error_code.is_empty()
                    || !payload.error_code.bytes().enumerate().all(|(index, byte)| {
                        if index == 0 {
                            byte.is_ascii_uppercase()
                        } else {
                            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                        }
                    })
                    || !bounded(&payload.message, 4096)
                {
                    return Err(invalid("runtime-error payload is invalid"));
                }
            }
            Self::UsageReported(payload) => {
                if payload.input_tokens > MAX_JCS_SAFE_INTEGER
                    || payload.output_tokens > MAX_JCS_SAFE_INTEGER
                {
                    return Err(invalid("usage payload exceeds the JCS-safe integer range"));
                }
            }
            Self::WorkspaceChanges(payload) => {
                if payload.paths.len() > 4096
                    || payload.paths.iter().any(|path| match path {
                        LosslessPath::Utf8(value) => !bounded(value, 4096),
                        LosslessPath::Bytes { bytes } => {
                            if bytes.len() > 8192 {
                                return true;
                            }
                            let Ok(decoded) =
                                base64::engine::general_purpose::STANDARD.decode(bytes)
                            else {
                                return true;
                            };
                            base64::engine::general_purpose::STANDARD.encode(decoded) != *bytes
                        }
                    })
                {
                    return Err(invalid("workspace-changes payload exceeds bounds"));
                }
            }
            Self::WriterStateChanged(payload) => {
                if !valid_writer_state(&payload.previous)
                    || !valid_writer_state(&payload.current)
                    || payload.writer_generation > MAX_JCS_SAFE_INTEGER
                    || payload
                        .writer_run_id
                        .is_some_and(|identity| identity.get_version_num() != 7)
                {
                    return Err(invalid("writer payload is invalid"));
                }
            }
            Self::RecoveryRequired(payload) => {
                if !bounded(&payload.reason, 4096) {
                    return Err(invalid("recovery reason exceeds its bound"));
                }
            }
            Self::CommandStarted(payload) => validate_command(&payload.command)?,
            Self::CommandCompleted(payload) => {
                validate_command(&payload.command)?;
                if payload
                    .exit_status
                    .is_some_and(|status| status.unsigned_abs() > MAX_JCS_SAFE_INTEGER)
                {
                    return Err(invalid(
                        "command exit status exceeds the JCS-safe integer range",
                    ));
                }
            }
            Self::DiagnosticReported(payload) => {
                if !bounded(&payload.message, 4096) {
                    return Err(invalid("diagnostic message exceeds its bound"));
                }
            }
            Self::GenerationChanged(payload) => {
                if payload.server_epoch == 0
                    || payload.server_epoch > MAX_JCS_SAFE_INTEGER
                    || payload.run_generation > MAX_JCS_SAFE_INTEGER
                {
                    return Err(invalid(
                        "generation event requires JCS-safe positive epochs",
                    ));
                }
            }
            Self::ReasoningSuppressed(payload) => {
                if !bounded(&payload.method, 256)
                    || payload.byte_length > MAX_JCS_SAFE_INTEGER
                    || !is_sha256(&payload.sha256)
                    || payload.reason != "reasoning_content_not_retained"
                {
                    return Err(invalid("reasoning suppression metadata is invalid"));
                }
            }
        }
        Ok(())
    }
}

impl FinalResponse {
    fn validate(&self, run_id: Uuid) -> Result<(), EventError> {
        match self {
            Self::Inline { text } => {
                if !bounded(text, 1024 * 1024) {
                    return Err(invalid("inline final response exceeds 1 MiB"));
                }
            }
            Self::Artifact { artifact } => artifact.validate(run_id)?,
        }
        Ok(())
    }
}

impl ArtifactMetadata {
    fn validate(&self, run_id: Uuid) -> Result<(), EventError> {
        if self.schema_version != 1
            || self.artifact_id.get_version_num() != 7
            || self.run_id != run_id
            || self.kind != "final_response"
            || self.visibility != "observer"
            || self.interaction_request_id.is_some()
            || self.media_type != "text/markdown"
            || self.byte_length > 32 * 1024 * 1024
            || !is_sha256(&self.sha256)
            || !is_microsecond_utc_timestamp(&self.created_at)
            || self.retention != "run_lifetime"
            || !matches!(
                self.integrity.as_str(),
                "verified" | "unverified" | "failed"
            )
        {
            return Err(invalid("final-response artifact metadata is invalid"));
        }
        Ok(())
    }
}

pub(crate) fn from_payload(payload: &LosslessJson) -> Result<ClientEventRecord, EventError> {
    const EVENT_FIELDS: [&str; 13] = [
        "schema_version",
        "event_schema_version",
        "cursor",
        "event_id",
        "timestamp",
        "workspace_id",
        "run_id",
        "thread_id",
        "turn_id",
        "server_key",
        "server_epoch",
        "type",
        "payload",
    ];
    let LosslessJson::Object(entries) = payload else {
        return Err(invalid("client event payload is not an object"));
    };
    if entries.len() != EVENT_FIELDS.len()
        || entries
            .iter()
            .any(|(name, _)| !EVENT_FIELDS.contains(&name.as_str()))
    {
        return Err(invalid(
            "client event has missing or unknown top-level fields",
        ));
    }
    let bytes = canonicalize(payload)
        .map_err(|error| invalid(&format!("client event is not canonical: {error}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| invalid(&format!("client event is invalid: {error}")))
}

fn invalid(reason: &str) -> EventError {
    EventError(reason.to_owned())
}

fn valid_run_state(value: &str) -> bool {
    serde_json::from_value::<crate::domain::RunLifecycle>(serde_json::Value::String(
        value.to_owned(),
    ))
    .is_ok()
}

fn valid_turn_state(value: &str) -> bool {
    matches!(
        value,
        "reserved"
            | "accepted"
            | "running"
            | "waiting_interaction"
            | "interrupting"
            | "completed"
            | "failed"
            | "interrupted"
            | "outcome_unknown"
    )
}

fn valid_writer_state(value: &str) -> bool {
    matches!(
        value,
        "none" | "reserved" | "active" | "handoff_prepared" | "releasing" | "blocked_unknown"
    )
}

fn validate_command(command: &[String]) -> Result<(), EventError> {
    if command.len() > 256 || command.iter().any(|argument| !bounded(argument, 4096)) {
        return Err(invalid("command payload exceeds its bounds"));
    }
    Ok(())
}

fn bounded(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
