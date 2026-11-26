//! Tier-1 structured driver for Agy ACP (`agy acp --stdio`).

use async_trait::async_trait;
use dolgorae_spi::{
    AgentDriver, DriverCapabilities, DriverError, DriverSession, ResumeSpec, SessionSpec,
};

/// Stable driver identifier for the Agy ACP driver.
pub const DRIVER_ID: &str = "agy-acp";

/// Driver implementation for `agy acp --stdio` (implemented in V2-T03).
#[derive(Debug, Default, Clone)]
pub struct AgyAcpDriver;

impl AgyAcpDriver {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentDriver for AgyAcpDriver {
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
            native_output_schema: false,
            supports_mcp_injection: true,
            supports_resume: true,
        })
    }

    async fn start_session(
        &self,
        _spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, DriverError> {
        Err(DriverError::Unavailable(
            "agy-acp driver implementation pending V2-T03".to_owned(),
        ))
    }

    async fn resume_session(
        &self,
        _spec: ResumeSpec,
    ) -> Result<Box<dyn DriverSession>, DriverError> {
        Err(DriverError::Unavailable(
            "agy-acp driver implementation pending V2-T03".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_id_is_stable() {
        let driver = AgyAcpDriver::new();
        assert_eq!(driver.driver_id(), "agy-acp");
    }
}
