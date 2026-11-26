//! Tier-1 structured driver for Codex app-server (`codex app-server --stdio`).

use async_trait::async_trait;
use dolgorae_spi::{
    AgentDriver, DriverCapabilities, DriverError, DriverSession, ResumeSpec, SessionSpec,
};

/// Stable driver identifier for the Codex app-server driver.
pub const DRIVER_ID: &str = "codex-app";

/// Driver implementation for `codex app-server --stdio` (implemented in V2-T03).
#[derive(Debug, Default, Clone)]
pub struct CodexAppDriver;

impl CodexAppDriver {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentDriver for CodexAppDriver {
    fn driver_id(&self) -> &'static str {
        DRIVER_ID
    }

    async fn probe(&self) -> Result<DriverCapabilities, DriverError> {
        Ok(DriverCapabilities {
            driver_id: DRIVER_ID.to_owned(),
            cli_version: None,
            available: false,
            unavailable_reason: Some("driver implementation pending V2-T03".to_owned()),
            models: Vec::new(),
            supports_effort: true,
            supports_interactive_approval: true,
            native_output_schema: true,
            supports_mcp_injection: true,
            supports_resume: true,
        })
    }

    async fn start_session(
        &self,
        _spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, DriverError> {
        Err(DriverError::Unavailable(
            "codex-app driver implementation pending V2-T03".to_owned(),
        ))
    }

    async fn resume_session(
        &self,
        _spec: ResumeSpec,
    ) -> Result<Box<dyn DriverSession>, DriverError> {
        Err(DriverError::Unavailable(
            "codex-app driver implementation pending V2-T03".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_id_is_stable() {
        let driver = CodexAppDriver::new();
        assert_eq!(driver.driver_id(), "codex-app");
    }
}
