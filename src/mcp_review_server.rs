//! Private stdio MCP adapter for external Specialist Review hosts.
//!
//! TASK-011 selected `mcp_unavailable`, so this adapter intentionally exposes
//! no model-facing review tool. The process still implements the bounded MCP
//! lifecycle so hosts can discover that disposition without receiving any
//! adapter-owned workspace, profile, identity, credential, or storage state.

use crate::machine::MachineError;
use crate::mcp_review::McpReviewDisposition;
use crate::semantic::prepare_reviewer;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::io::{self, BufRead, BufReader};
use std::path::Path;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MCP_PROTOCOL_VERSION: &str = "2026-07-28";

/// Checked TASK-011 result. Changing this value requires replacing the pinned
/// host evidence artifact and implementing the replay-safe call path first.
const PINNED_DISPOSITION: McpReviewDisposition = McpReviewDisposition::McpUnavailable;

struct AdapterBinding {
    _workspace_sha256: [u8; 32],
    _profile_sha256: [u8; 32],
}

impl AdapterBinding {
    fn validate(workspace: &Path, profile: &str) -> Result<Self, MachineError> {
        let prepared = prepare_reviewer(
            Some(workspace),
            profile,
            "Independent read-only working-tree review",
        )?;
        let canonical = prepared.view.canonical_path.to_path_buf()?;
        Ok(Self {
            _workspace_sha256: Sha256::digest(canonical.as_os_str().as_encoded_bytes()).into(),
            _profile_sha256: Sha256::digest(profile.as_bytes()).into(),
        })
    }

    #[cfg(test)]
    fn canary(workspace: &str, profile: &str) -> Self {
        Self {
            _workspace_sha256: Sha256::digest(workspace.as_bytes()).into(),
            _profile_sha256: Sha256::digest(profile.as_bytes()).into(),
        }
    }
}

pub fn serve_stdio(workspace: &Path, profile: &str) -> Result<(), MachineError> {
    if !workspace.is_absolute() {
        return Err(MachineError::invalid_argument(
            "--workspace",
            "the private MCP binding requires an absolute workspace path",
        ));
    }
    let binding = AdapterBinding::validate(workspace, profile)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(
        BufReader::new(stdin.lock()),
        stdout.lock(),
        binding,
        MAX_FRAME_BYTES,
    )
    .map_err(|_| {
        MachineError::new(
            "MCP_TRANSPORT_FAILED",
            "private Specialist Review MCP transport failed",
            false,
            json!({}),
        )
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordState {
    Complete,
    Oversized,
    PartialEof,
    Eof,
}

fn read_record<R: BufRead>(
    reader: &mut R,
    frame: &mut Vec<u8>,
    limit: usize,
) -> io::Result<RecordState> {
    frame.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if frame.is_empty() {
                RecordState::Eof
            } else {
                RecordState::PartialEof
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if frame.len().saturating_add(consumed) > limit {
            reader.consume(consumed);
            if newline.is_none() {
                drain_record(reader)?;
            }
            frame.clear();
            return Ok(RecordState::Oversized);
        }
        frame.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            frame.pop();
            return Ok(RecordState::Complete);
        }
    }
}

fn drain_record<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(());
        }
    }
}

fn serve<R: BufRead, W: io::Write>(
    mut reader: R,
    mut writer: W,
    _binding: AdapterBinding,
    limit: usize,
) -> io::Result<()> {
    debug_assert_eq!(PINNED_DISPOSITION, McpReviewDisposition::McpUnavailable);
    let mut initialized = false;
    let mut frame = Vec::with_capacity(4096);
    loop {
        match read_record(&mut reader, &mut frame, limit)? {
            RecordState::Eof => return Ok(()),
            RecordState::PartialEof => {
                write_response(&mut writer, error(Value::Null, -32700, "partial MCP frame"))?;
                return Ok(());
            }
            RecordState::Oversized => {
                write_response(
                    &mut writer,
                    error(Value::Null, -32600, "MCP frame too large"),
                )?;
                continue;
            }
            RecordState::Complete => {}
        }

        let request: Value = match serde_json::from_slice(&frame) {
            Ok(Value::Object(value)) => Value::Object(value),
            _ => {
                write_response(&mut writer, error(Value::Null, -32700, "invalid JSON-RPC"))?;
                continue;
            }
        };
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str);
        let notification = id.is_none();

        if request.get("jsonrpc") != Some(&Value::String("2.0".to_owned())) || method.is_none() {
            if !notification {
                write_response(
                    &mut writer,
                    error(
                        id.unwrap_or(Value::Null),
                        -32600,
                        "invalid JSON-RPC request",
                    ),
                )?;
            }
            continue;
        }
        let method = method.expect("checked above");
        if notification {
            continue;
        }
        let id = id.expect("request has id");
        if !matches!(id, Value::Null | Value::Number(_) | Value::String(_)) {
            write_response(
                &mut writer,
                error(Value::Null, -32600, "invalid JSON-RPC request id"),
            )?;
            continue;
        }
        let response = match method {
            "initialize"
                if request
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    == Some(MCP_PROTOCOL_VERSION) =>
            {
                initialized = true;
                success(
                    id,
                    json!({
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {
                            "name": "dolgorae-specialist-review",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "instructions": "Specialist Review is available through the Machine CLI carrier."
                    }),
                )
            }
            "initialize" => error(id, -32602, "unsupported MCP protocol version"),
            _ if !initialized => error(id, -32002, "MCP server is not initialized"),
            "tools/list" => success(id, json!({"tools": []})),
            "tools/call" => unavailable(id),
            _ => error(id, -32601, "method not found"),
        };
        write_response(&mut writer, response)?;
    }
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &'static str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn unavailable(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": "dolgorae_review is unavailable under the selected host disposition",
            "data": {
                "code": "MCP_REVIEW_UNAVAILABLE",
                "retryable": false,
                "supportedCarrier": "machine_cli"
            }
        }
    })
}

fn write_response<W: io::Write>(writer: &mut W, response: Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, &response)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const WORKSPACE_CANARY: &str = "/private/workspace-CANARY";
    const PROFILE_CANARY: &str = "profile-CANARY";

    fn exchange(input: &[u8], limit: usize) -> Vec<Value> {
        let mut output = Vec::new();
        serve(
            Cursor::new(input),
            &mut output,
            AdapterBinding::canary(WORKSPACE_CANARY, PROFILE_CANARY),
            limit,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains(WORKSPACE_CANARY));
        assert!(!text.contains(PROFILE_CANARY));
        assert!(!text.contains("sqlite"));
        assert!(!text.contains("socket"));
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn selected_disposition_initializes_but_advertises_no_tool() {
        let responses = exchange(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2026-07-28\"}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
            MAX_FRAME_BYTES,
        );
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[0]["result"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        assert_eq!(responses[1]["result"]["tools"], json!([]));
        assert!(
            !serde_json::to_string(&responses)
                .unwrap()
                .contains("externalRequestRef")
        );
    }

    #[test]
    fn calls_fail_before_allocating_any_review_identity() {
        let responses = exchange(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2026-07-28\"}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"dolgorae_review\",\"arguments\":{\"secret\":\"RAW-CANARY\",\"hidden_reasoning\":\"HIDDEN-CANARY\"}}}\n",
            MAX_FRAME_BYTES,
        );
        assert_eq!(
            responses[1]["error"]["data"]["code"],
            "MCP_REVIEW_UNAVAILABLE"
        );
        assert_eq!(responses[1]["error"]["data"]["retryable"], false);
        assert!(
            !serde_json::to_string(&responses)
                .unwrap()
                .contains("RAW-CANARY")
        );
        assert!(
            !serde_json::to_string(&responses)
                .unwrap()
                .contains("HIDDEN-CANARY")
        );
        assert!(responses[1].pointer("/result/review_id").is_none());
    }

    #[test]
    fn malformed_oversized_and_partial_frames_are_bounded() {
        let malformed = exchange(b"not-json\n", 64);
        assert_eq!(malformed[0]["error"]["code"], -32700);

        let oversized = exchange(b"{xxxxxxxxxxxxxxxx}\n", 8);
        assert_eq!(oversized[0]["error"]["code"], -32600);

        let partial = exchange(b"{\"jsonrpc\":\"2.0\"}", 64);
        assert_eq!(partial[0]["error"]["message"], "partial MCP frame");
    }

    #[test]
    fn pre_initialize_requests_fail_and_cancellation_notifications_are_silent() {
        let responses = exchange(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":1}}\n",
            MAX_FRAME_BYTES,
        );
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["error"]["code"], -32002);
    }

    #[test]
    fn initialized_notification_cannot_bypass_handshake_and_ids_are_scalar() {
        let responses = exchange(
            b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2026-07-28\"}}\n",
            MAX_FRAME_BYTES,
        );
        assert_eq!(responses[0]["error"]["code"], -32002);
        assert_eq!(responses[1]["error"]["code"], -32600);
        assert_eq!(responses[1]["id"], Value::Null);
    }
}
