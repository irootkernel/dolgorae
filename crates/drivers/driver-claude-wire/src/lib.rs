//! Tier-1 structured driver for Claude Code (`stream-json` + `--permission-prompts host`).
//!
//! ## Synthesized `turn_id`
//!
//! Claude Code `stream-json` has no native per-turn identifier. This driver
//! synthesizes a UUID v7 `turn_id` on each `send_turn` and attaches it to all
//! emitted events until `result` completes that turn.
//!
//! ## Security & `ReadOnly` Policy Model
//!
//! - **Zero secrets in `argv`**: MCP server definitions (including
//!   `DOLGORAE_ACTOR_TOKEN`) live only in a `0600` file inside a `0700` private
//!   temporary directory passed to `--mcp-config <path>`, deleted on `close()`
//!   and `Drop`. `--bare` is never passed.
//! - **Unsandboxed evaluation (`os_sandboxed = false`)**: `can_use_tool` prompts
//!   are evaluated via `dolgorae_spi::evaluate_interaction(..., false)`. Under
//!   `ReadOnly`, the driver passes `--permission-mode manual` plus
//!   `--disallowedTools Edit Write NotebookEdit MultiEdit` (avoiding `plan` mode
//!   so review turns finish with `AssistantMessage`), hard-denies
//!   `FileChangeApproval` under every policy, relies on human approval for
//!   unsandboxed `Bash` under `ReadOnly + Interactive`, and denies unsandboxed
//!   commands under `AutoApproveScoped`.
//! - **Backpressure**: events are forwarded via `.send().await` and never dropped.

mod session;
mod wire;

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use async_trait::async_trait;
use dolgorae_spi::{
    AccessMode, AgentDriver, ApprovalPolicy, DriverCapabilities, DriverError, DriverSession,
    McpServerInjection, ResumeSpec, SessionSpec,
};
use serde_json::{Map, Value, json};
use tokio::process::Command;
use uuid::Uuid;

use crate::session::ClaudeSession;

/// Stable driver identifier for the Claude Code `stream-json` driver.
pub const DRIVER_ID: &str = "claude-wire";

/// Environment variable overriding the Claude Code executable path.
pub const CLAUDE_BIN_ENV: &str = "DOLGORAE_CLAUDE_BIN";

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const READ_ONLY_DISALLOWED: [&str; 4] = ["Edit", "Write", "NotebookEdit", "MultiEdit"];

/// Tier-1 structured driver for Claude Code `stream-json` wire protocol.
#[derive(Debug, Clone)]
pub struct ClaudeWireDriver {
    binary: PathBuf,
}

/// Convenience alias for `ClaudeWireDriver`.
pub type ClaudeDriver = ClaudeWireDriver;

impl Default for ClaudeWireDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeWireDriver {
    /// Resolves `DOLGORAE_CLAUDE_BIN` or falls back to `"claude"`.
    pub fn new() -> Self {
        let bin = std::env::var_os(CLAUDE_BIN_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("claude"));
        Self { binary: bin }
    }

    /// Binds the driver to an explicit executable path (used for fake-stdio tests).
    pub fn with_binary(binary: PathBuf) -> Self {
        Self { binary }
    }

    fn caps(
        cli_version: Option<String>,
        available: bool,
        unavailable_reason: Option<String>,
    ) -> DriverCapabilities {
        DriverCapabilities {
            driver_id: DRIVER_ID.to_owned(),
            cli_version,
            available,
            unavailable_reason,
            models: Vec::new(),
            supports_effort: true,
            supports_interactive_approval: true,
            native_output_schema: true,
            supports_mcp_injection: true,
            supports_resume: true,
        }
    }
}

#[async_trait]
impl AgentDriver for ClaudeWireDriver {
    fn driver_id(&self) -> &'static str {
        DRIVER_ID
    }

    async fn probe(&self) -> Result<DriverCapabilities, DriverError> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let bin = self.binary.display();

        let out = match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Ok(Self::caps(None, false, Some(format!("exec '{bin}': {e}"))));
            }
            Err(_) => {
                return Ok(Self::caps(None, false, Some(format!("timeout '{bin}'"))));
            }
        };

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            let reason = if err.is_empty() {
                format!("'{bin} --version' exited with {}", out.status)
            } else {
                format!("'{bin} --version' failed ({}): {err}", out.status)
            };
            return Ok(Self::caps(None, false, Some(reason)));
        }

        let first = |b: &[u8]| {
            String::from_utf8_lossy(b)
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_owned())
        };
        let ver = first(&out.stdout).or_else(|| first(&out.stderr));
        Ok(Self::caps(ver, true, None))
    }

    async fn start_session(&self, s: SessionSpec) -> Result<Box<dyn DriverSession>, DriverError> {
        let opts = LaunchOptions {
            workspace: s.workspace,
            role_instructions: s.role_instructions,
            model: s.model,
            effort: s.effort,
            access: s.access,
            approval_policy: s.approval_policy,
            output_schema: s.output_schema,
            mcp_servers: s.mcp_servers,
            session_mode: SessionMode::Start(Uuid::now_v7().to_string()),
        };
        Ok(Box::new(ClaudeSession::spawn(&self.binary, opts).await?))
    }

    async fn resume_session(&self, s: ResumeSpec) -> Result<Box<dyn DriverSession>, DriverError> {
        let opts = LaunchOptions {
            workspace: s.workspace,
            role_instructions: s.role_instructions,
            model: s.model,
            effort: s.effort,
            access: s.access,
            approval_policy: s.approval_policy,
            output_schema: s.output_schema,
            mcp_servers: s.mcp_servers,
            session_mode: SessionMode::Resume(s.provider_session_id),
        };
        Ok(Box::new(ClaudeSession::spawn(&self.binary, opts).await?))
    }
}

pub(crate) enum SessionMode {
    Start(String),
    Resume(String),
}

impl SessionMode {
    pub(crate) fn session_id(&self) -> &str {
        match self {
            Self::Start(id) | Self::Resume(id) => id,
        }
    }
}

pub(crate) struct LaunchOptions {
    pub workspace: PathBuf,
    pub role_instructions: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: AccessMode,
    pub approval_policy: ApprovalPolicy,
    pub output_schema: Option<Value>,
    pub mcp_servers: Vec<McpServerInjection>,
    pub session_mode: SessionMode,
}

/// Owns a `0700` temporary directory containing a `0600` `--mcp-config` file.
pub(crate) struct McpConfigGuard {
    dir_path: PathBuf,
    file_path: PathBuf,
    cleaned: AtomicBool,
}

impl McpConfigGuard {
    pub(crate) fn create(servers: &[McpServerInjection]) -> Result<Self, DriverError> {
        let dir_path = std::env::temp_dir().join(format!("dolgorae-claude-mcp-{}", Uuid::now_v7()));
        let mut db = DirBuilder::new();
        #[cfg(unix)]
        db.mode(0o700);
        db.create(&dir_path)?;
        #[cfg(unix)]
        fs::set_permissions(&dir_path, fs::Permissions::from_mode(0o700))?;

        let file_path = dir_path.join("mcp-config.json");
        let mut map = Map::new();
        for srv in servers {
            let env: Map<String, Value> = srv
                .env
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            map.insert(
                srv.name.clone(),
                json!({"type": "stdio", "command": srv.command.to_string_lossy(), "args": srv.args, "env": env}),
            );
        }
        let bytes = serde_json::to_vec(&json!({"mcpServers": map}))
            .map_err(|e| DriverError::InvalidConfig(format!("encode mcp config: {e}")))?;

        let mut oo = OpenOptions::new();
        oo.write(true).create_new(true);
        #[cfg(unix)]
        oo.mode(0o600);
        let res = oo.open(&file_path).and_then(|mut f| {
            f.write_all(&bytes)?;
            f.flush()?;
            #[cfg(unix)]
            fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600))?;
            Ok(())
        });
        if let Err(err) = res {
            let _ = fs::remove_file(&file_path);
            let _ = fs::remove_dir(&dir_path);
            return Err(DriverError::Io(err));
        }
        Ok(Self {
            dir_path,
            file_path,
            cleaned: AtomicBool::new(false),
        })
    }

    pub(crate) fn file_path(&self) -> &Path {
        &self.file_path
    }

    pub(crate) fn cleanup(&self) {
        if !self.cleaned.swap(true, Ordering::SeqCst) {
            let _ = fs::remove_file(&self.file_path);
            let _ = fs::remove_dir(&self.dir_path);
        }
    }
}

impl Drop for McpConfigGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

pub(crate) fn build_launch_args(
    opts: &LaunchOptions,
    mcp_config_path: &Path,
) -> Result<Vec<String>, DriverError> {
    let base = [
        "-p",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--replay-user-messages",
        "--permission-prompts",
        "host",
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        "manual",
    ];
    let mut args: Vec<String> = base.into_iter().map(str::to_owned).collect();

    for (flag, val) in [
        ("--model", opts.model.as_deref()),
        ("--effort", opts.effort.as_deref()),
        ("--append-system-prompt", opts.role_instructions.as_deref()),
    ] {
        if let Some(v) = val.filter(|s| !s.is_empty()) {
            args.extend([flag.to_owned(), v.to_owned()]);
        }
    }
    if let Some(schema) = &opts.output_schema {
        let encoded = serde_json::to_string(schema)
            .map_err(|e| DriverError::InvalidConfig(format!("encode output_schema: {e}")))?;
        args.extend(["--json-schema".to_owned(), encoded]);
    }
    match &opts.session_mode {
        SessionMode::Start(sid) => {
            args.extend(["--session-id".to_owned(), sid.clone()]);
        }
        SessionMode::Resume(rid) => {
            if rid.is_empty() {
                return Err(DriverError::InvalidConfig(
                    "provider_session_id must not be empty".to_owned(),
                ));
            }
            args.extend(["--resume".to_owned(), rid.clone()]);
        }
    }
    args.extend([
        "--strict-mcp-config".to_owned(),
        "--mcp-config".to_owned(),
        mcp_config_path.to_string_lossy().into_owned(),
    ]);
    if opts.access == AccessMode::ReadOnly {
        args.push("--disallowedTools".to_owned());
        args.extend(READ_ONLY_DISALLOWED.into_iter().map(str::to_owned));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_id_is_claude_wire() {
        let driver = ClaudeWireDriver::new();
        assert_eq!(driver.driver_id(), "claude-wire");
        let alias = ClaudeDriver::with_binary(PathBuf::from("/nonexistent/claude"));
        assert_eq!(alias.driver_id(), "claude-wire");
    }

    #[test]
    fn launch_args_never_pass_bare_and_add_disallowed_tools_for_readonly() {
        let opts = LaunchOptions {
            workspace: PathBuf::from("/tmp/ws"),
            role_instructions: Some("be concise".to_owned()),
            model: Some("sonnet".to_owned()),
            effort: Some("high".to_owned()),
            access: AccessMode::ReadOnly,
            approval_policy: ApprovalPolicy::Interactive,
            output_schema: Some(json!({"type": "object"})),
            mcp_servers: Vec::new(),
            session_mode: SessionMode::Resume("sess-42".to_owned()),
        };
        let args = build_launch_args(&opts, Path::new("/tmp/p/mcp.json")).expect("args");
        assert!(!args.iter().any(|a| a == "--bare"));
        assert!(!args.iter().any(|a| a == "--session-id"));
        assert!(args.iter().any(|a| a == "--verbose"));
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-mode", "manual"])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-prompts", "host"])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-prompt-tool", "stdio"])
        );
        assert!(args.windows(2).any(|w| w == ["--resume", "sess-42"]));
        assert!(args.iter().any(|a| a == "--disallowedTools"));
        for tool in READ_ONLY_DISALLOWED {
            assert!(args.iter().any(|a| a == tool));
        }

        let start_opts = LaunchOptions {
            session_mode: SessionMode::Start("uuid-start-1".to_owned()),
            ..opts
        };
        let start_args =
            build_launch_args(&start_opts, Path::new("/tmp/p/mcp.json")).expect("start args");
        assert!(
            start_args
                .windows(2)
                .any(|w| w == ["--session-id", "uuid-start-1"])
        );
        assert!(!start_args.iter().any(|a| a == "--resume"));
    }

    #[tokio::test]
    async fn probe_missing_binary_returns_unavailable_ok() {
        let driver = ClaudeWireDriver::with_binary(PathBuf::from("/nonexistent-dolgorae-claude"));
        let caps = driver.probe().await.expect("probe returns Ok");
        assert!(!caps.available);
        assert!(caps.unavailable_reason.is_some());
        assert_eq!(caps.driver_id, "claude-wire");
        assert!(caps.native_output_schema);
        assert!(caps.supports_effort);
        assert!(caps.supports_interactive_approval);
        assert!(caps.supports_mcp_injection);
        assert!(caps.supports_resume);
    }
}
