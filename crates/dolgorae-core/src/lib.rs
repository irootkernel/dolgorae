//! Core orchestration engine for Dolgorae v2.
//!
//! Owns Role/Persona catalogs, unified Session/Actor/Delegation state,
//! durable SQLite event log and mailbox storage, cursor/revision minting,
//! optimistic concurrency checks, and bounded artifact storage.

pub use dolgorae_spi::{
    AccessMode, AgentDriver, ApprovalPolicy, DriverCapabilities, DriverError, DriverSession,
    InteractionAnswer, InteractionKind, InteractionRequest, NormalizedEvent, ResumeSpec,
    SessionSpec, TurnAck, TurnInput, TurnTerminalStatus,
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

    #[test]
    fn size_bounds_match_v2_contract() {
        assert_eq!(MAX_INLINE_BYTES, 262_144);
        assert_eq!(MAX_ARTIFACT_BYTES, 67_108_864);
        assert_eq!(MAX_PAGE_BYTES, 4_194_304);
        assert_eq!(MAX_RECEIVE_BYTES, 8_388_608);
    }
}
