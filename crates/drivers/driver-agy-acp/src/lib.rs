//! Tier-1 structured driver for Agy ACP (`agy acp --stdio --process-per-session`).
//!
//! - Starts the NDJSON stdout reader before `initialize` so interleaved
//!   `session/update` notifications during `session/new` never deadlock RPC waiters.
//! - Forwards events with bounded `.send().await` backpressure (never drops silently).
//! - Sends `mcpServers` (including `DOLGORAE_ACTOR_TOKEN`) exclusively over stdin
//!   JSON-RPC with `env` encoded as `[{"name": k, "value": v}]` (zero secrets in argv).
//! - Advertises `native_output_schema = false`, injecting schema guidance via
//!   `_meta.antigravity.systemPrompt.append` and per-turn prompt text.
//! - Evaluates reverse `session/request_permission` via `dolgorae_spi::evaluate_interaction`
//!   with `os_sandboxed = false`: `ReadOnly` unconditionally hard-denies file mutations,
//!   `AutoApproveScoped` denies unsandboxed commands, and `ReadOnly + Interactive` treats
//!   the human caller as the approval authority for commands.
//! - Runs each session in its own process group (`process_group(0)`). `close()` sends
//!   `SIGTERM` -> grace -> `SIGKILL` to `-<pgid>`; `Drop` sends non-blocking `SIGTERM`
//!   plus detached reaping. Grandchildren calling `setsid(2)` escape PGID signaling.

mod protocol;
mod session;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use dolgorae_spi::{
    AgentDriver, DriverCapabilities, DriverError, DriverSession, ResumeSpec, SessionSpec,
};
use tokio::process::Command;

pub use session::AgySession;

/// Stable driver identifier for the Agy ACP driver.
pub const DRIVER_ID: &str = "agy-acp";

/// Convenience alias for `AgyAcpDriver`.
pub type AgyDriver = AgyAcpDriver;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Driver implementation for `agy acp --stdio --process-per-session`.
#[derive(Debug, Clone)]
pub struct AgyAcpDriver {
    binary: PathBuf,
}

impl Default for AgyAcpDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl AgyAcpDriver {
    /// Creates a driver using `DOLGORAE_AGY_BIN` if set and non-empty, or `"agy"`.
    pub fn new() -> Self {
        let binary = std::env::var_os("DOLGORAE_AGY_BIN")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("agy"));
        Self { binary }
    }

    /// Creates a driver targeting an explicit binary path (useful for testing).
    pub fn with_binary(binary: PathBuf) -> Self {
        Self { binary }
    }
}

#[async_trait]
impl AgentDriver for AgyAcpDriver {
    fn driver_id(&self) -> &'static str {
        DRIVER_ID
    }

    async fn probe(&self) -> Result<DriverCapabilities, DriverError> {
        let bin = self.binary.display();
        let fut = Command::new(&self.binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        let (available, cli_version, unavailable_reason) =
            match tokio::time::timeout(PROBE_TIMEOUT, fut).await {
                Ok(Ok(out)) if out.status.success() => {
                    let s_out = String::from_utf8_lossy(&out.stdout);
                    let s_err = String::from_utf8_lossy(&out.stderr);
                    let ver = s_out
                        .lines()
                        .chain(s_err.lines())
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .map(str::to_owned);
                    (true, ver, None)
                }
                Ok(Ok(out)) => {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                    let sep = if err.is_empty() { "" } else { ": " };
                    (
                        false,
                        None,
                        Some(format!(
                            "'{bin} --version' exited with {}{sep}{err}",
                            out.status
                        )),
                    )
                }
                Ok(Err(e)) => (false, None, Some(format!("failed to execute '{bin}': {e}"))),
                Err(_) => (false, None, Some(format!("timed out probing '{bin}'"))),
            };

        Ok(DriverCapabilities {
            driver_id: DRIVER_ID.to_owned(),
            cli_version,
            available,
            unavailable_reason,
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
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, DriverError> {
        Ok(Box::new(AgySession::start(&self.binary, spec, None).await?))
    }

    async fn resume_session(
        &self,
        spec: ResumeSpec,
    ) -> Result<Box<dyn DriverSession>, DriverError> {
        let resume_id = spec.provider_session_id;
        let session_spec = SessionSpec {
            workspace: spec.workspace,
            role_instructions: spec.role_instructions,
            model: spec.model,
            effort: spec.effort,
            access: spec.access,
            approval_policy: spec.approval_policy,
            output_schema: spec.output_schema,
            mcp_servers: spec.mcp_servers,
        };
        let sess = AgySession::start(&self.binary, session_spec, Some(resume_id)).await?;
        Ok(Box::new(sess))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_id_and_alias_are_consistent() {
        let driver = AgyAcpDriver::with_binary(PathBuf::from("agy-custom"));
        assert_eq!(driver.driver_id(), "agy-acp");
        let alias = AgyDriver::with_binary(PathBuf::from("agy-custom"));
        assert_eq!(alias.driver_id(), "agy-acp");
    }
}
