//! Stdio NDJSON JSON-RPC 2.0 MCP server implementation.

use std::path::PathBuf;

use dolgorae_api::UdsClient;
use dolgorae_core::{CORE_VERSION, CoreError, MAX_INLINE_BYTES};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::McpMode;
use crate::tools::{dispatch_tool, list_tool_schemas};

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_PROTOCOL_VERSION: &str = "2025-03-26";

/// Startup and I/O errors for the stdio MCP server.
#[derive(Debug, Error)]
pub enum McpError {
    #[error("scoped actor MCP mode requires non-empty DOLGORAE_ACTOR_TOKEN")]
    MissingActorToken,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Configuration for `McpServer`.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub mode: McpMode,
    pub socket_path: PathBuf,
    pub actor_token: Option<Zeroizing<String>>,
    pub self_session_id: Option<String>,
}

/// Dual-mode stdio JSON-RPC 2.0 MCP server backed by `dolgorae serve`.
pub struct McpServer {
    mode: McpMode,
    client: UdsClient,
    self_session_id: Option<String>,
}

impl McpServer {
    /// Constructs a new `McpServer`, failing closed if `ScopedActor` mode has a
    /// missing or empty `actor_token`.
    pub fn new(config: McpServerConfig) -> Result<Self, McpError> {
        let mut client = UdsClient::new(config.socket_path);
        if config.mode == McpMode::ScopedActor {
            let token = config
                .actor_token
                .filter(|t| !t.trim().is_empty())
                .ok_or(McpError::MissingActorToken)?;
            client = client.with_bearer_token(token);
        }
        Ok(Self {
            mode: config.mode,
            client,
            self_session_id: config.self_session_id.filter(|s| !s.trim().is_empty()),
        })
    }

    pub fn mode(&self) -> McpMode {
        self.mode
    }

    /// Runs the NDJSON JSON-RPC 2.0 loop over `reader` and `writer` until EOF.
    ///
    /// Writes ONLY valid JSON-RPC frames to `writer`; never writes diagnostics
    /// to stdout.
    pub async fn run_stdio<R, W>(&self, mut reader: R, mut writer: W) -> Result<(), McpError>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(response) = self.handle_raw_line(trimmed).await {
                let serialized = serde_json::to_string(&response)
                    .unwrap_or_else(|_| r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"serialization failed"}}"#.to_owned());
                writer.write_all(serialized.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
        }
        Ok(())
    }

    /// Handles one JSON-RPC 2.0 request or notification line.
    pub async fn handle_raw_line(&self, raw: &str) -> Option<Value> {
        let parsed: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(err) => {
                return Some(jsonrpc_error(
                    Value::Null,
                    -32700,
                    format!("parse error: {err}"),
                ));
            }
        };

        let id = parsed.get("id").cloned();
        let Some(method) = parsed.get("method").and_then(Value::as_str) else {
            return id.map(|req_id| {
                jsonrpc_error(req_id, -32600, "invalid request: missing method".to_owned())
            });
        };
        let params = parsed.get("params").cloned().unwrap_or_else(|| json!({}));

        // JSON-RPC notifications have no `id` and must not elicit a response.
        if id.is_none() {
            tracing::debug!(method, "received MCP notification");
            return None;
        }
        let req_id = id.unwrap_or(Value::Null);

        match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str);
                let negotiated = match requested {
                    Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
                    _ => DEFAULT_PROTOCOL_VERSION,
                };
                Some(jsonrpc_ok(
                    req_id,
                    json!({
                        "protocolVersion": negotiated,
                        "capabilities": { "tools": {} },
                        "serverInfo": {
                            "name": "dolgorae",
                            "version": CORE_VERSION,
                        }
                    }),
                ))
            }
            "ping" => Some(jsonrpc_ok(req_id, json!({}))),
            "tools/list" => Some(jsonrpc_ok(
                req_id,
                json!({ "tools": list_tool_schemas(self.mode) }),
            )),
            "tools/call" => {
                let Some(tool_name) = params.get("name").and_then(Value::as_str) else {
                    return Some(jsonrpc_error(
                        req_id,
                        -32602,
                        "missing params.name".to_owned(),
                    ));
                };
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));

                let result = match dispatch_tool(
                    self.mode,
                    &self.client,
                    self.self_session_id.as_deref(),
                    tool_name,
                    args,
                )
                .await
                {
                    Ok(value) => format_tool_success(&self.client, value).await,
                    Err(err) => format_tool_error(err),
                };
                Some(jsonrpc_ok(req_id, result))
            }
            other => Some(jsonrpc_error(
                req_id,
                -32601,
                format!("method not found: {other}"),
            )),
        }
    }
}

async fn format_tool_success(client: &UdsClient, value: Value) -> Value {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
    if text.len() > MAX_INLINE_BYTES {
        return match client
            .post_artifact_bytes("application/json", text.into_bytes())
            .await
        {
            Ok(artifact) => {
                let spilled = json!({
                    "spilled": true,
                    "artifact": artifact,
                    "note": "response exceeded 256 KiB inline cap; read slices via artifact_read",
                });
                json!({
                    "content": [{ "type": "text", "text": spilled.to_string() }],
                    "isError": false,
                })
            }
            Err(err) => format_tool_error(err),
        };
    }
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    })
}

fn format_tool_error(err: CoreError) -> Value {
    let text = serde_json::to_string(&err.to_envelope()).unwrap_or_else(|_| {
        json!({ "error": { "code": err.code.as_str(), "message": err.message } }).to_string()
    });
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true,
    })
}

fn jsonrpc_ok(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn jsonrpc_error(id: Value, code: i32, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}
