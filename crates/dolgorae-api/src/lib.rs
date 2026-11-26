//! Local UDS HTTP/1.1 JSON + SSE gateway for Dolgorae v2.
//!
//! Enforces exclusive daemon locking, same-user peer credentials on the Unix
//! domain socket, per-request scoped actor bearer tokens, and projects
//! `dolgorae-core` onto all 19 OpenAPI v2 routes.

pub mod auth;
pub mod client;
pub mod server;
pub mod sse;

pub use auth::{ApiError, resolve_caller, verify_peer_uid};
pub use client::{SseByteBuffer, SseFrame, SseStream, UdsClient, url_encode};
pub use dolgorae_core::{
    CORE_VERSION, MAX_ARTIFACT_BYTES, MAX_INLINE_BYTES, MAX_PAGE_BYTES, MAX_RECEIVE_BYTES,
};
pub use server::{BoundUdsServer, DaemonLock, ServerConfig};

/// API protocol version identifier.
pub const API_VERSION: &str = "v2";

#[cfg(test)]
mod tests {
    use super::*;
    use dolgorae_core::ErrorCode;

    #[test]
    fn api_version_is_v2() {
        assert_eq!(API_VERSION, "v2");
    }

    #[test]
    fn peer_uid_mismatch_returns_unauthorized() {
        assert!(verify_peer_uid(501, 501).is_ok());
        let err = verify_peer_uid(502, 501).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);
    }

    #[test]
    fn url_encode_escapes_query_characters() {
        assert_eq!(
            url_encode("text/plain; charset=utf-8"),
            "text%2Fplain%3B%20charset%3Dutf-8"
        );
    }

    #[test]
    fn sse_byte_buffer_preserves_multibyte_utf8_across_one_byte_chunks() {
        let expected_data = "{\"text\":\"안녕하세요 돌고래 🐬🚀\"}";
        let raw = format!("id: 7\nevent: assistant_message\ndata: {expected_data}\n\n");
        let mut parser = SseByteBuffer::new();
        let mut frames = Vec::new();
        // Feed 1 byte at a time, splitting every multi-byte Korean & emoji codepoint!
        for b in raw.as_bytes() {
            parser.push_bytes(&[*b]);
            while let Some(f) = parser.pop_frame() {
                frames.push(f);
            }
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id.as_deref(), Some("7"));
        assert_eq!(frames[0].event.as_deref(), Some("assistant_message"));
        assert_eq!(frames[0].data, expected_data);
        assert!(!frames[0].data.contains('\u{FFFD}'));
    }
}
