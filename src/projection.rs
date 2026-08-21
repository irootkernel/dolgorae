use crate::audit::{AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH};
pub use crate::domain::{Access as ProjectedAccess, RunLifecycle};
use crate::jcs::LosslessJson;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerHead {
    pub sequence: u64,
    pub hash: String,
}

impl Default for LedgerHead {
    fn default() -> Self {
        Self {
            sequence: 0,
            hash: GENESIS_PREVIOUS_HASH.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectedWriterAuthority {
    None,
    Reserved,
    Active,
    HandoffPrepared,
    Releasing,
    BlockedUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStateProjection {
    pub schema_version: u32,
    pub run_id: Uuid,
    pub lifecycle: RunLifecycle,
    pub run_generation: u64,
    pub thread_id: Option<String>,
    pub active_turn_id: Option<String>,
    pub latest_turn_id: Option<String>,
    pub pending_requests: Vec<String>,
    pub access: ProjectedAccess,
    pub writer_authority: ProjectedWriterAuthority,
    pub default_effort: Option<String>,
    pub last_event_cursor: Option<String>,
    pub ledger_head: LedgerHead,
}

impl RunStateProjection {
    pub(crate) fn empty(run_id: Uuid) -> Self {
        Self {
            schema_version: 1,
            run_id,
            lifecycle: RunLifecycle::Starting,
            run_generation: 0,
            thread_id: None,
            active_turn_id: None,
            latest_turn_id: None,
            pending_requests: Vec::new(),
            access: ProjectedAccess::Unknown,
            writer_authority: ProjectedWriterAuthority::None,
            default_effort: None,
            last_event_cursor: None,
            ledger_head: LedgerHead::default(),
        }
    }
}

pub(crate) fn project_next(
    run_id: Uuid,
    current: &RunStateProjection,
    record: &AuditRecord,
    event_cursor: impl Fn(&AuditRecord) -> Result<String, String>,
) -> Result<RunStateProjection, String> {
    let mut state = current.clone();
    let mut pending = state.pending_requests.iter().cloned().collect();
    apply_record(run_id, &mut state, &mut pending, record, &event_cursor)?;
    state.pending_requests = pending.into_iter().collect();
    validate(&state)?;
    Ok(state)
}

fn apply_record(
    run_id: Uuid,
    state: &mut RunStateProjection,
    pending: &mut BTreeSet<String>,
    record: &AuditRecord,
    event_cursor: &impl Fn(&AuditRecord) -> Result<String, String>,
) -> Result<(), String> {
    if record.run_id() != run_id {
        return Err("replay crossed run identity".to_owned());
    }
    state.run_generation = state.run_generation.max(record.run_generation());
    match record.kind() {
        AuditKind::RunCreated => {
            state.lifecycle = RunLifecycle::Starting;
            if let Some(access) = payload_string(record.payload(), "initial_access") {
                state.access = parse_access(&access)?;
            }
            state.default_effort = payload_string(record.payload(), "default_effort");
            if state
                .default_effort
                .as_deref()
                .is_some_and(|value| !bounded(value, 64))
            {
                return Err("default effort exceeds its projection bound".to_owned());
            }
        }
        AuditKind::ThreadBound => {
            let thread = required_payload_string(record.payload(), "thread_id")?;
            if !bounded(&thread, 256) {
                return Err("thread identity exceeds its projection bound".to_owned());
            }
            state.thread_id = Some(thread);
        }
        AuditKind::TurnStarted => {
            let turn = required_payload_string(record.payload(), "turn_id")?;
            if !bounded(&turn, 256) {
                return Err("turn identity exceeds its projection bound".to_owned());
            }
            state.active_turn_id = Some(turn.clone());
            state.latest_turn_id = Some(turn);
            state.lifecycle = RunLifecycle::Running;
        }
        AuditKind::TurnTerminal => {
            let turn = required_payload_string(record.payload(), "turn_id")?;
            if !bounded(&turn, 256) {
                return Err("turn identity exceeds its projection bound".to_owned());
            }
            if state.active_turn_id.as_deref() == Some(turn.as_str()) {
                state.active_turn_id = None;
            }
            state.latest_turn_id = Some(turn);
            state.lifecycle = if pending.is_empty() {
                RunLifecycle::Idle
            } else {
                RunLifecycle::WaitingInteraction
            };
        }
        AuditKind::LifecycleTransition => {
            state.lifecycle =
                parse_lifecycle(&required_payload_string(record.payload(), "current")?)?;
            if matches!(
                state.lifecycle,
                RunLifecycle::Closed | RunLifecycle::StartFailed | RunLifecycle::OutcomeUnknown
            ) {
                state.active_turn_id = None;
            }
        }
        AuditKind::RunGenerationStarted | AuditKind::RunGenerationStopped => {
            state.run_generation = record.run_generation();
        }
        AuditKind::InteractionOpened => {
            let request = required_payload_string(record.payload(), "request_id")?;
            if !bounded(&request, 256) || pending.len() >= 4096 {
                return Err("pending interaction exceeds projection bounds".to_owned());
            }
            pending.insert(request);
            state.lifecycle = RunLifecycle::WaitingInteraction;
        }
        AuditKind::InteractionResolved => {
            let request = required_payload_string(record.payload(), "request_id")?;
            if !bounded(&request, 256) {
                return Err("resolved interaction exceeds its projection bound".to_owned());
            }
            pending.remove(&request);
            state.lifecycle = if pending.is_empty() {
                if state.active_turn_id.is_some() {
                    RunLifecycle::Running
                } else {
                    RunLifecycle::Idle
                }
            } else {
                RunLifecycle::WaitingInteraction
            };
        }
        AuditKind::WriterAcquired => {
            state.writer_authority = ProjectedWriterAuthority::Active;
            state.access = ProjectedAccess::Write;
        }
        AuditKind::WriterReleased => {
            state.writer_authority = ProjectedWriterAuthority::None;
            state.access = ProjectedAccess::Read;
        }
        AuditKind::WriterHandoffRequested => {
            state.writer_authority = ProjectedWriterAuthority::HandoffPrepared;
            state.access = ProjectedAccess::Transitioning;
        }
        AuditKind::WriterHandoffCancelled => {
            state.writer_authority = ProjectedWriterAuthority::Active;
            state.access = ProjectedAccess::Write;
        }
        AuditKind::WriterHandoffCompleted => {
            state.writer_authority = ProjectedWriterAuthority::None;
            state.access = ProjectedAccess::Read;
        }
        AuditKind::Reconciliation => state.lifecycle = RunLifecycle::ReconciliationRequired,
        AuditKind::ClientEvent => state.last_event_cursor = Some(event_cursor(record)?),
        AuditKind::StartFailed => {
            state.lifecycle = RunLifecycle::StartFailed;
            state.active_turn_id = None;
        }
        AuditKind::OutcomeUnknown => {
            state.lifecycle = RunLifecycle::OutcomeUnknown;
            state.active_turn_id = None;
        }
        _ => {}
    }
    state.ledger_head = LedgerHead {
        sequence: record.sequence(),
        hash: record.hash().to_owned(),
    };
    Ok(())
}

pub(crate) fn validate(projection: &RunStateProjection) -> Result<(), String> {
    if projection.schema_version != 1 || projection.run_id.get_version_num() != 7 {
        return Err("projection schema or run identity is invalid".to_owned());
    }
    if !is_sha256(&projection.ledger_head.hash)
        || (projection.ledger_head.sequence == 0
            && projection.ledger_head.hash != GENESIS_PREVIOUS_HASH)
    {
        return Err("projection ledger head is invalid".to_owned());
    }
    if projection
        .thread_id
        .as_deref()
        .is_some_and(|value| !bounded(value, 256))
        || projection
            .active_turn_id
            .as_deref()
            .is_some_and(|value| !bounded(value, 256))
        || projection
            .latest_turn_id
            .as_deref()
            .is_some_and(|value| !bounded(value, 256))
        || projection
            .default_effort
            .as_deref()
            .is_some_and(|value| !bounded(value, 64))
    {
        return Err("projection identity or effort exceeds its bound".to_owned());
    }
    if projection.pending_requests.len() > 4096
        || projection
            .pending_requests
            .iter()
            .any(|value| !bounded(value, 256))
        || projection
            .pending_requests
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err("pending requests are not bounded, sorted, and unique".to_owned());
    }
    if projection.active_turn_id.is_some() && projection.active_turn_id != projection.latest_turn_id
    {
        return Err("active turn is not the latest turn".to_owned());
    }
    if matches!(
        projection.lifecycle,
        RunLifecycle::Closed | RunLifecycle::StartFailed | RunLifecycle::OutcomeUnknown
    ) && projection.active_turn_id.is_some()
    {
        return Err("terminal projection retains an active turn".to_owned());
    }
    if projection.lifecycle == RunLifecycle::WaitingInteraction
        && projection.pending_requests.is_empty()
    {
        return Err("waiting projection has no pending interaction".to_owned());
    }
    if projection.writer_authority == ProjectedWriterAuthority::Active
        && projection.access != ProjectedAccess::Write
    {
        return Err("active writer projection is not write-capable".to_owned());
    }
    if projection
        .last_event_cursor
        .as_deref()
        .is_some_and(|cursor| {
            cursor.parse::<u64>().map_or(true, |value| {
                value == 0 || value > projection.ledger_head.sequence || cursor.starts_with('0')
            })
        })
    {
        return Err("event cursor exceeds or disagrees with the ledger head".to_owned());
    }
    Ok(())
}

fn payload_string(payload: &LosslessJson, name: &str) -> Option<String> {
    let LosslessJson::Object(entries) = payload else {
        return None;
    };
    entries.iter().find_map(|(key, value)| {
        (key == name)
            .then_some(value)
            .and_then(|value| match value {
                LosslessJson::String(value) => Some(value.clone()),
                _ => None,
            })
    })
}

fn required_payload_string(payload: &LosslessJson, name: &str) -> Result<String, String> {
    payload_string(payload, name)
        .ok_or_else(|| format!("audit payload is missing required string {name}"))
}

fn parse_lifecycle(value: &str) -> Result<RunLifecycle, String> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| format!("unknown lifecycle projection {value}"))
}

fn parse_access(value: &str) -> Result<ProjectedAccess, String> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| format!("unknown access projection {value}"))
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
