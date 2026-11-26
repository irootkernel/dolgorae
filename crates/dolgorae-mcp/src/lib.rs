//! Stdio MCP adapter for Dolgorae v2.
//!
//! Supports both External Host Mode (15 tools) and Scoped Actor Mode (7 tools,
//! `--scoped` with mandatory `DOLGORAE_ACTOR_TOKEN` fail-closed authentication
//! over the local `dolgorae serve` Unix socket daemon).

pub mod server;
pub mod tools;

pub use dolgorae_core::CORE_VERSION;
pub use server::{McpError, McpServer, McpServerConfig};
pub use tools::{clamp_wait_timeout, list_tool_schemas};

/// Operating mode for the stdio MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMode {
    ExternalHost,
    ScopedActor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use zeroize::Zeroizing;

    #[test]
    fn modes_and_tool_counts_match_contract() {
        assert_ne!(McpMode::ExternalHost, McpMode::ScopedActor);
        let ext = list_tool_schemas(McpMode::ExternalHost);
        let scoped = list_tool_schemas(McpMode::ScopedActor);
        assert_eq!(ext.len(), 15);
        assert_eq!(scoped.len(), 7);

        let scoped_names: Vec<&str> = scoped.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            scoped_names,
            vec![
                "actor_delegate",
                "actor_child_status",
                "actor_child_wait",
                "actor_child_cancel",
                "actor_mailbox_send",
                "actor_mailbox_read",
                "actor_artifact_read",
            ]
        );
    }

    #[test]
    fn scoped_mode_fails_closed_without_token() {
        let missing = McpServer::new(McpServerConfig {
            mode: McpMode::ScopedActor,
            socket_path: PathBuf::from("/tmp/none.sock"),
            actor_token: None,
            self_session_id: None,
        });
        assert!(matches!(missing, Err(McpError::MissingActorToken)));

        let empty = McpServer::new(McpServerConfig {
            mode: McpMode::ScopedActor,
            socket_path: PathBuf::from("/tmp/none.sock"),
            actor_token: Some(Zeroizing::new("   ".to_owned())),
            self_session_id: None,
        });
        assert!(matches!(empty, Err(McpError::MissingActorToken)));
    }

    #[test]
    fn wait_timeout_clamps_below_client_tool_timeout() {
        assert_eq!(clamp_wait_timeout(None), 25_000);
        assert_eq!(clamp_wait_timeout(Some(0)), 1);
        assert_eq!(clamp_wait_timeout(Some(10_000)), 10_000);
        assert_eq!(clamp_wait_timeout(Some(300_000)), 55_000);
    }
}
