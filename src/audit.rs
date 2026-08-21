use crate::jcs::{
    JcsError, LosslessJson, PayloadRepresentation, canonicalize, represent_payload, sha256_hex,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const HASH_SCHEME: &str = "sha256-jcs-v1";
pub const GENESIS_PREVIOUS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

macro_rules! audit_kinds {
    ($($variant:ident => $value:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum AuditKind { $($variant),+ }

        impl AuditKind {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }
    };
}

audit_kinds! {
    WorkspaceInitialized => "workspace_initialized",
    RunCreated => "run_created",
    WriteContinuationCreated => "write_continuation_created",
    TurnIntent => "turn_intent",
    ThreadBound => "thread_bound",
    TurnStarted => "turn_started",
    TurnTerminal => "turn_terminal",
    LifecycleTransition => "lifecycle_transition",
    RunGenerationStarted => "run_generation_started",
    RunGenerationStopped => "run_generation_stopped",
    AppServerRequest => "app_server_request",
    AppServerResponse => "app_server_response",
    AppServerNotification => "app_server_notification",
    ApprovalRequested => "approval_requested",
    ApprovalDecided => "approval_decided",
    InteractionOpened => "interaction_opened",
    InteractionResolved => "interaction_resolved",
    ClientEvent => "client_event",
    ControllerReset => "controller_reset",
    ReasoningContentSuppressed => "reasoning_content_suppressed",
    WriterAcquired => "writer_acquired",
    WriterReleased => "writer_released",
    WriterHandoffRequested => "writer_handoff_requested",
    WriterHandoffCancelled => "writer_handoff_cancelled",
    WriterHandoffCompleted => "writer_handoff_completed",
    ProfileObserved => "profile_observed",
    IdempotencyReserved => "idempotency_reserved",
    Reconciliation => "reconciliation",
    CleanupIntent => "cleanup_intent",
    CleanupResult => "cleanup_result",
    LedgerTailRepaired => "ledger_tail_repaired",
    ProjectionRewound => "projection_rewound",
    PayloadUnrepresentable => "payload_unrepresentable",
    StartFailed => "start_failed",
    OutcomeUnknown => "outcome_unknown"
}

#[derive(Clone, Debug, PartialEq)]
pub struct AuditRecord {
    schema_version: u32,
    sequence: u64,
    timestamp: String,
    run_id: Uuid,
    run_generation: u64,
    kind: AuditKind,
    payload: LosslessJson,
    previous_hash: String,
    hash: String,
}

#[derive(Debug)]
pub enum AuditError {
    InvalidSequence,
    InvalidTimestamp,
    InvalidHash,
    InvalidPayload,
    InvalidStructure,
    NonCanonical,
    Canonicalization(JcsError),
}

impl std::fmt::Display for AuditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSequence => formatter.write_str("audit sequence must start at one"),
            Self::InvalidTimestamp => formatter.write_str(
                "audit timestamp must be UTC RFC 3339 with exactly six fractional digits",
            ),
            Self::InvalidHash => {
                formatter.write_str("audit previous hash must be lowercase SHA-256")
            }
            Self::InvalidPayload => formatter
                .write_str("payload_unrepresentable must contain only bounded non-secret metadata"),
            Self::InvalidStructure => formatter.write_str("audit record structure is invalid"),
            Self::NonCanonical => formatter.write_str("audit record line is not canonical JCS"),
            Self::Canonicalization(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AuditError {}

impl From<JcsError> for AuditError {
    fn from(value: JcsError) -> Self {
        Self::Canonicalization(value)
    }
}

impl AuditRecord {
    pub fn from_canonical_line(line: &[u8]) -> Result<Self, AuditError> {
        if line.is_empty() || line.contains(&b'\n') || line.contains(&b'\r') {
            return Err(AuditError::InvalidStructure);
        }
        let text = std::str::from_utf8(line).map_err(|_| AuditError::InvalidStructure)?;
        let value = crate::jcs::parse(text)?;
        if canonicalize(&value)? != line {
            return Err(AuditError::NonCanonical);
        }
        let LosslessJson::Object(entries) = value else {
            return Err(AuditError::InvalidStructure);
        };
        if entries.len() != 9 {
            return Err(AuditError::InvalidStructure);
        }
        let field = |name: &str| {
            entries
                .iter()
                .find_map(|(key, value)| (key == name).then_some(value))
                .ok_or(AuditError::InvalidStructure)
        };
        let number = |name: &str| match field(name)? {
            LosslessJson::Number(value) => value
                .parse::<u64>()
                .map_err(|_| AuditError::InvalidStructure),
            _ => Err(AuditError::InvalidStructure),
        };
        let string = |name: &str| match field(name)? {
            LosslessJson::String(value) => Ok(value.clone()),
            _ => Err(AuditError::InvalidStructure),
        };
        if number("schema_version")? != 1 {
            return Err(AuditError::InvalidStructure);
        }
        let sequence = number("sequence")?;
        if sequence == 0 {
            return Err(AuditError::InvalidSequence);
        }
        let timestamp = string("timestamp")?;
        if !is_microsecond_utc_timestamp(&timestamp) {
            return Err(AuditError::InvalidTimestamp);
        }
        let run_id =
            Uuid::parse_str(&string("run_id")?).map_err(|_| AuditError::InvalidStructure)?;
        if run_id.get_version_num() != 7 {
            return Err(AuditError::InvalidStructure);
        }
        let run_generation = number("run_generation")?;
        let kind = serde_json::from_value::<AuditKind>(serde_json::Value::String(string("kind")?))
            .map_err(|_| AuditError::InvalidStructure)?;
        let payload = field("payload")?.clone();
        let previous_hash = string("previous_hash")?;
        let hash = string("hash")?;
        if !is_sha256(&previous_hash) || !is_sha256(&hash) {
            return Err(AuditError::InvalidHash);
        }
        if kind == AuditKind::PayloadUnrepresentable && !is_unrepresentable_payload(&payload) {
            return Err(AuditError::InvalidPayload);
        }
        let record = Self {
            schema_version: 1,
            sequence,
            timestamp,
            run_id,
            run_generation,
            kind,
            payload,
            previous_hash,
            hash,
        };
        if !record.verify_hash() {
            return Err(AuditError::InvalidHash);
        }
        Ok(record)
    }

    pub fn new(
        sequence: u64,
        timestamp: impl Into<String>,
        run_id: Uuid,
        run_generation: u64,
        kind: AuditKind,
        payload: LosslessJson,
        previous_hash: impl Into<String>,
    ) -> Result<Self, AuditError> {
        if requires_representation(kind) {
            return Err(AuditError::InvalidPayload);
        }
        Self::new_inner(
            sequence,
            timestamp,
            run_id,
            run_generation,
            kind,
            payload,
            previous_hash,
        )
    }

    pub fn new_represented(
        sequence: u64,
        timestamp: impl Into<String>,
        run_id: Uuid,
        run_generation: u64,
        source_kind: AuditKind,
        raw_payload: &[u8],
        previous_hash: impl Into<String>,
    ) -> Result<Self, AuditError> {
        if source_kind == AuditKind::PayloadUnrepresentable {
            return Err(AuditError::InvalidPayload);
        }
        let (kind, payload) = represent_audit_payload(source_kind, raw_payload);
        Self::new_inner(
            sequence,
            timestamp,
            run_id,
            run_generation,
            kind,
            payload,
            previous_hash,
        )
    }

    fn new_inner(
        sequence: u64,
        timestamp: impl Into<String>,
        run_id: Uuid,
        run_generation: u64,
        kind: AuditKind,
        payload: LosslessJson,
        previous_hash: impl Into<String>,
    ) -> Result<Self, AuditError> {
        if sequence == 0 {
            return Err(AuditError::InvalidSequence);
        }
        let timestamp = timestamp.into();
        if !is_microsecond_utc_timestamp(&timestamp) {
            return Err(AuditError::InvalidTimestamp);
        }
        let previous_hash = previous_hash.into();
        if !is_sha256(&previous_hash) {
            return Err(AuditError::InvalidHash);
        }
        if kind == AuditKind::PayloadUnrepresentable && !is_unrepresentable_payload(&payload) {
            return Err(AuditError::InvalidPayload);
        }
        let mut record = Self {
            schema_version: 1,
            sequence,
            timestamp,
            run_id,
            run_generation,
            kind,
            payload,
            previous_hash,
            hash: String::new(),
        };
        record.hash = sha256_hex(&canonicalize(&record.lossless(false))?);
        Ok(record)
    }

    pub fn canonical_line(&self) -> Result<Vec<u8>, AuditError> {
        if !self.verify_hash() {
            return Err(AuditError::InvalidHash);
        }
        if self.kind == AuditKind::PayloadUnrepresentable
            && !is_unrepresentable_payload(&self.payload)
        {
            return Err(AuditError::InvalidPayload);
        }
        let mut bytes = canonicalize(&self.lossless(true))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    #[must_use]
    pub fn verify_hash(&self) -> bool {
        is_sha256(&self.hash)
            && canonicalize(&self.lossless(false))
                .map(|bytes| sha256_hex(&bytes) == self.hash)
                .unwrap_or(false)
    }

    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn run_id(&self) -> Uuid {
        self.run_id
    }

    #[must_use]
    pub const fn run_generation(&self) -> u64 {
        self.run_generation
    }

    #[must_use]
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }

    #[must_use]
    pub fn previous_hash(&self) -> &str {
        &self.previous_hash
    }

    #[must_use]
    pub const fn kind(&self) -> AuditKind {
        self.kind
    }

    #[must_use]
    pub const fn payload(&self) -> &LosslessJson {
        &self.payload
    }

    fn lossless(&self, include_hash: bool) -> LosslessJson {
        let mut entries = vec![
            (
                "schema_version".to_owned(),
                LosslessJson::Number(self.schema_version.to_string()),
            ),
            (
                "sequence".to_owned(),
                LosslessJson::Number(self.sequence.to_string()),
            ),
            (
                "timestamp".to_owned(),
                LosslessJson::String(self.timestamp.clone()),
            ),
            (
                "run_id".to_owned(),
                LosslessJson::String(self.run_id.to_string()),
            ),
            (
                "run_generation".to_owned(),
                LosslessJson::Number(self.run_generation.to_string()),
            ),
            (
                "kind".to_owned(),
                LosslessJson::String(self.kind.as_str().to_owned()),
            ),
            ("payload".to_owned(), self.payload.clone()),
            (
                "previous_hash".to_owned(),
                LosslessJson::String(self.previous_hash.clone()),
            ),
        ];
        if include_hash {
            entries.push(("hash".to_owned(), LosslessJson::String(self.hash.clone())));
        }
        LosslessJson::Object(entries)
    }
}

const fn requires_representation(kind: AuditKind) -> bool {
    matches!(
        kind,
        AuditKind::AppServerRequest
            | AuditKind::AppServerResponse
            | AuditKind::AppServerNotification
            | AuditKind::ClientEvent
    )
}

#[must_use]
pub fn represent_audit_payload(source_kind: AuditKind, input: &[u8]) -> (AuditKind, LosslessJson) {
    match represent_payload(input) {
        PayloadRepresentation::Represented { value, .. } => (source_kind, value),
        PayloadRepresentation::Unrepresentable(metadata) => (
            AuditKind::PayloadUnrepresentable,
            LosslessJson::Object(vec![
                (
                    "source_kind".to_owned(),
                    LosslessJson::String(source_kind.as_str().to_owned()),
                ),
                (
                    "observed_byte_length".to_owned(),
                    LosslessJson::Number(metadata.observed_byte_length.to_string()),
                ),
                (
                    "raw_sha256".to_owned(),
                    LosslessJson::String(metadata.raw_sha256),
                ),
                (
                    "json_pointer".to_owned(),
                    metadata
                        .json_pointer
                        .map_or(LosslessJson::Null, LosslessJson::String),
                ),
                (
                    "reason".to_owned(),
                    LosslessJson::String(metadata.reason.to_owned()),
                ),
            ]),
        ),
    }
}

fn is_unrepresentable_payload(payload: &LosslessJson) -> bool {
    let LosslessJson::Object(entries) = payload else {
        return false;
    };
    if entries.len() != 5 {
        return false;
    }
    let get = |name: &str| {
        entries
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value))
    };
    let source_kind = match get("source_kind") {
        Some(LosslessJson::String(value)) => value,
        _ => return false,
    };
    if source_kind == AuditKind::PayloadUnrepresentable.as_str()
        || serde_json::from_value::<AuditKind>(serde_json::Value::String(source_kind.clone()))
            .is_err()
    {
        return false;
    }
    if !matches!(get("observed_byte_length"), Some(LosslessJson::Number(value)) if value.parse::<u64>().is_ok())
        || !matches!(get("raw_sha256"), Some(LosslessJson::String(value)) if is_sha256(value))
        || !matches!(
            get("json_pointer"),
            Some(LosslessJson::Null | LosslessJson::String(_))
        )
        || !matches!(
            get("reason"),
            Some(LosslessJson::String(value))
                if matches!(
                    value.as_str(),
                    "raw_payload_too_large"
                        | "payload_not_utf8"
                        | "duplicate_object_member"
                        | "invalid_json"
                        | "canonicalization_failed"
                        | "represented_payload_too_large"
                )
        )
    {
        return false;
    }
    let pointer = get("json_pointer");
    !matches!(pointer, Some(LosslessJson::String(value)) if !value.is_empty() && !value.starts_with('/'))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn is_microsecond_utc_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 27
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[26] != b'Z'
        || bytes
            .iter()
            .enumerate()
            .filter(|(index, _)| ![4, 7, 10, 13, 16, 19, 26].contains(index))
            .any(|(_, byte)| !byte.is_ascii_digit())
    {
        return false;
    }
    let number = |start: usize, end: usize| {
        value[start..end]
            .parse::<u32>()
            .expect("digit-only timestamp component")
    };
    let year = number(0, 4);
    let month = number(5, 7);
    let day = number(8, 10);
    let hour = number(11, 13);
    let minute = number(14, 16);
    let second = number(17, 19);
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day) && hour < 24 && minute < 60 && second < 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_hash_omits_only_hash_and_line_is_canonical() {
        let run_id = Uuid::parse_str("018f0c6a-7b01-7abc-8def-0123456789ab").unwrap();
        let record = AuditRecord::new(
            1,
            "2026-08-21T12:34:56.123456Z",
            run_id,
            0,
            AuditKind::RunCreated,
            LosslessJson::Object(vec![(
                "workspace_id".to_owned(),
                LosslessJson::String("a".repeat(64)),
            )]),
            GENESIS_PREVIOUS_HASH,
        )
        .unwrap();
        assert!(record.verify_hash());
        let line = record.canonical_line().unwrap();
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(std::str::from_utf8(&line).unwrap().contains(record.hash()));
    }

    #[test]
    fn timestamp_shape_and_closed_kind_are_strict() {
        assert!(is_microsecond_utc_timestamp("2024-02-29T23:59:59.000001Z"));
        assert!(!is_microsecond_utc_timestamp("2023-02-29T23:59:59.000001Z"));
        assert!(!is_microsecond_utc_timestamp("2026-08-21T12:34:56Z"));
        assert!(serde_json::from_str::<AuditKind>("\"future_kind\"").is_err());
    }

    #[test]
    fn unrepresentable_kind_rejects_arbitrary_or_recursive_source_payloads() {
        let run_id = Uuid::parse_str("018f0c6a-7b01-7abc-8def-0123456789ab").unwrap();
        for payload in [
            LosslessJson::Object(vec![(
                "raw_bytes".to_owned(),
                LosslessJson::String("TOP-SECRET".to_owned()),
            )]),
            LosslessJson::Object(vec![
                (
                    "source_kind".to_owned(),
                    LosslessJson::String("payload_unrepresentable".to_owned()),
                ),
                (
                    "observed_byte_length".to_owned(),
                    LosslessJson::Number("1".to_owned()),
                ),
                (
                    "raw_sha256".to_owned(),
                    LosslessJson::String("a".repeat(64)),
                ),
                ("json_pointer".to_owned(), LosslessJson::Null),
                (
                    "reason".to_owned(),
                    LosslessJson::String("invalid_json".to_owned()),
                ),
            ]),
        ] {
            assert!(matches!(
                AuditRecord::new(
                    1,
                    "2026-08-21T12:34:56.123456Z",
                    run_id,
                    0,
                    AuditKind::PayloadUnrepresentable,
                    payload,
                    GENESIS_PREVIOUS_HASH,
                ),
                Err(AuditError::InvalidPayload)
            ));
        }
    }

    #[test]
    fn wire_payloads_cannot_bypass_representation() {
        let run_id = Uuid::parse_str("018f0c6a-7b01-7abc-8def-0123456789ab").unwrap();
        assert!(matches!(
            AuditRecord::new(
                1,
                "2026-08-21T12:34:56.123456Z",
                run_id,
                0,
                AuditKind::AppServerResponse,
                LosslessJson::Object(vec![(
                    "password".to_owned(),
                    LosslessJson::String("TOP-SECRET".to_owned()),
                )]),
                GENESIS_PREVIOUS_HASH,
            ),
            Err(AuditError::InvalidPayload)
        ));
        let represented = AuditRecord::new_represented(
            1,
            "2026-08-21T12:34:56.123456Z",
            run_id,
            0,
            AuditKind::AppServerResponse,
            br#"{"password":"TOP-SECRET"}"#,
            GENESIS_PREVIOUS_HASH,
        )
        .unwrap();
        let line = String::from_utf8(represented.canonical_line().unwrap()).unwrap();
        assert!(!line.contains("TOP-SECRET"));
        assert!(line.contains("$dolgorae_redacted"));
    }
}
