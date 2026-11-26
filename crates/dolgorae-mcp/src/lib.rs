//! Stdio MCP adapter for Dolgorae v2.
//!
//! Supports both External Host Mode and Scoped Actor Mode (`--scoped` with
//! per-actor token authentication over the local daemon).

pub use dolgorae_core::CORE_VERSION;

/// Operating mode for the stdio MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMode {
    ExternalHost,
    ScopedActor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_are_distinct() {
        assert_ne!(McpMode::ExternalHost, McpMode::ScopedActor);
    }
}
