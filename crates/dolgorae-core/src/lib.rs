//! Core orchestration engine for Dolgorae v2.
//!
//! Owns Role/Persona catalogs, unified Session/Actor/Delegation state,
//! durable SQLite event log and mailbox storage, cursor/revision minting,
//! optimistic concurrency checks, and bounded artifact storage.

pub mod artifacts;
pub mod engine;
pub mod error;
pub mod roles;
pub mod schema;
pub mod store;
pub mod types;

pub use dolgorae_spi::{
    AccessMode, AgentDriver, ApprovalPolicy, AttachmentRef, DecisionKind, DecisionOption,
    DriverCapabilities, DriverError, DriverSession, InteractionAnswer, InteractionKind,
    InteractionRequest, McpServerInjection, ModelDescriptor, NormalizedEvent, ResumeSpec,
    SessionSpec, ToolActivityStatus, TurnAck, TurnInput, TurnTerminalStatus, UsageMetrics,
};

pub use artifacts::ArtifactStore;
pub use engine::CoreEngine;
pub use error::{CoreError, ErrorBody, ErrorCode, ErrorEnvelope};
pub use roles::{
    MAX_ROLE_FILE_BYTES, list_roles, load_role, parse_role_markdown, validate_role_name,
};
pub use schema::validate_json_schema;
pub use store::Store;
pub use types::{
    ActorScope, AnswerInteractionRequest, AnswerInteractionResponse, ArtifactRef, CallerContext,
    CapabilitiesSnapshot, CoreConfig, CreateSessionRequest, Cursor, DEFAULT_MAX_DELEGATION_DEPTH,
    DEFAULT_MAX_RETAINED_EVENTS, HARD_MAX_DELEGATION_DEPTH, InteractionList, InteractionRecord,
    InteractionStatus, MailboxMessage, MailboxPage, PostMailboxRequest, ReplayResult,
    ResumeSessionRequest, RoleDetail, RoleSource, RoleSummary, SchemaValidationResult,
    SessionEvent, SessionFilter, SessionPolicy, SessionSnapshot, SessionStatus, SizeBounds,
    SubmitTurnRequest, TimelineItem, TimelinePage, TurnAccepted, WaitMode, WaitSessionsRequest,
    WaitSessionsResponse, WriterLease, now_rfc3339,
};

/// Canonical inline payload ceiling (256 KiB).
pub const MAX_INLINE_BYTES: usize = 256 * 1024;

/// Canonical single artifact ceiling (64 MiB).
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

/// Canonical timeline page payload ceiling (4 MiB).
pub const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// Canonical HTTP request body ceiling (8 MiB).
pub const MAX_RECEIVE_BYTES: usize = 8 * 1024 * 1024;

/// Crate version reported by the v2 core engine.
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::Path;

    #[test]
    fn size_bounds_match_v2_contract() {
        assert_eq!(MAX_INLINE_BYTES, 262_144);
        assert_eq!(MAX_ARTIFACT_BYTES, 67_108_864);
        assert_eq!(MAX_PAGE_BYTES, 4_194_304);
        assert_eq!(MAX_RECEIVE_BYTES, 8_388_608);
    }

    #[test]
    fn cursor_parse_enforces_canonical_decimal_u64() {
        assert_eq!(Cursor::parse("0").unwrap(), Cursor(0));
        assert_eq!(Cursor::parse("1").unwrap(), Cursor(1));
        assert_eq!(
            Cursor::parse("18446744073709551615").unwrap(),
            Cursor(u64::MAX)
        );

        for bad in [
            "",
            "00",
            "01",
            "-1",
            "+1",
            "1.0",
            "abc",
            "18446744073709551616",
        ] {
            let err = Cursor::parse(bad).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::BadRequest,
                "expected BadRequest for {bad}"
            );
        }
    }

    #[test]
    fn json_schema_validator_handles_subset_rules() {
        let schema = json!({
            "type": "object",
            "required": ["ok", "items"],
            "additionalProperties": false,
            "properties": {
                "ok": { "const": true },
                "severity": { "enum": ["low", "high"] },
                "items": {
                    "type": "array",
                    "items": { "type": "integer" }
                }
            }
        });

        let ok = validate_json_schema(
            &schema,
            &json!({ "ok": true, "severity": "high", "items": [1, 2] }),
        );
        assert!(ok.validated, "unexpected errors: {:?}", ok.errors);

        let bad = validate_json_schema(
            &schema,
            &json!({ "ok": false, "severity": "critical", "items": ["x"], "extra": 1 }),
        );
        assert!(!bad.validated);
        assert!(
            bad.errors.len() >= 4,
            "expected multiple errors, got {:?}",
            bad.errors
        );
    }

    #[test]
    fn core_src_never_reads_home_env_or_dirs() {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden = ["var(\"HOME\")", "var_os(\"HOME\")", "dirs::"];
        for entry in fs::read_dir(&src_dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let content = fs::read_to_string(&path).expect("read rs file");
            let prod_only = content.split("#[cfg(test)]").next().unwrap_or(&content);
            for needle in forbidden {
                assert!(
                    !prod_only.contains(needle),
                    "forbidden home lookup '{needle}' found in {}",
                    path.display()
                );
            }
        }
    }
}
