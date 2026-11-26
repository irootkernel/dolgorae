//! Local UDS HTTP/1.1 JSON + SSE gateway for Dolgorae v2.
//!
//! Enforces same-user peer credentials on the Unix domain socket and projects
//! `dolgorae-core` session, timeline, interaction, artifact, and mailbox state
//! onto the OpenAPI v2 contract.

pub use dolgorae_core::{
    CORE_VERSION, MAX_ARTIFACT_BYTES, MAX_INLINE_BYTES, MAX_PAGE_BYTES, MAX_RECEIVE_BYTES,
};

/// API protocol version identifier.
pub const API_VERSION: &str = "v2";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_is_v2() {
        assert_eq!(API_VERSION, "v2");
    }
}
