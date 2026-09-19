//! Test-only stdio MCP probe for TASK-025. It is not the production Primary tool.

use crate::live_transport::{
    ExecutionLaneKind, McpCallContext, ProbeInvoke, ProbeLedger, ProbeOutcome,
    reject_model_identity,
};
use crate::machine::MachineError;
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Write};
use std::sync::Mutex;
use uuid::Uuid;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MCP_PROTOCOL_VERSIONS: [&str; 2] = ["2026-07-28", "2025-06-18"];
const TOOL_NAME: &str = "dolgorae_transport_probe";
const META_TURN: &str = "xyz.rootkernel.dolgorae/sourceTurnId";
const META_CALL: &str = "xyz.rootkernel.dolgorae/sourceToolCallId";
const META_IDEMPOTENCY: &str = "xyz.rootkernel.dolgorae/idempotencyKey";

pub struct ProbeProcessBinding {
    pub session_id: Uuid,
    pub run_id: Uuid,
    pub generation: u64,
    pub lane: ExecutionLaneKind,
}

impl ProbeProcessBinding {
    pub fn parse(
        session_id: &str,
        run_id: &str,
        generation: u64,
        dedicated: bool,
    ) -> Result<Self, MachineError> {
        Ok(Self {
            session_id: parse_uuid(session_id, "--session-id")?,
            run_id: parse_uuid(run_id, "--run-id")?,
            generation,
            lane: if dedicated {
                ExecutionLaneKind::DedicatedLane
            } else {
                ExecutionLaneKind::SharedProfileServer
            },
        })
    }
}

pub fn serve_stdio(binding: ProbeProcessBinding) -> Result<(), MachineError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(
        BufReader::new(stdin.lock()),
        stdout.lock(),
        binding,
        MAX_FRAME_BYTES,
        ProbeLedger::default(),
    )
    .map_err(|_| {
        MachineError::new(
            "MCP_TRANSPORT_FAILED",
            "private live-transport MCP probe failed",
            false,
            json!({}),
        )
    })
}

fn parse_uuid(value: &str, argument: &str) -> Result<Uuid, MachineError> {
    Uuid::parse_str(value)
        .map_err(|_| MachineError::invalid_argument(argument, "value must be a UUID"))
}

fn serve<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    binding: ProbeProcessBinding,
    limit: usize,
    ledger: ProbeLedger,
) -> io::Result<()> {
    let ledger = Mutex::new(ledger);
    let mut initialized = false;
    let mut frame = Vec::with_capacity(4096);
    loop {
        match read_record(&mut reader, &mut frame, limit)? {
            RecordState::Eof => return Ok(()),
            RecordState::PartialEof => {
                write_response(
                    &mut writer,
                    rpc_error(Value::Null, -32700, "partial MCP frame"),
                )?;
                return Ok(());
            }
            RecordState::Oversized => {
                write_response(
                    &mut writer,
                    rpc_error(Value::Null, -32600, "MCP frame too large"),
                )?;
                continue;
            }
            RecordState::Complete => {}
        }
        let request: Value = match serde_json::from_slice(&frame) {
            Ok(Value::Object(value)) => Value::Object(value),
            _ => {
                write_response(
                    &mut writer,
                    rpc_error(Value::Null, -32700, "invalid JSON-RPC"),
                )?;
                continue;
            }
        };
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str);
        if request.get("jsonrpc") != Some(&Value::String("2.0".to_owned())) || method.is_none() {
            if id.is_some() {
                write_response(
                    &mut writer,
                    rpc_error(
                        id.unwrap_or(Value::Null),
                        -32600,
                        "invalid JSON-RPC request",
                    ),
                )?;
            }
            continue;
        }
        if id.is_none() {
            continue;
        }
        let id = id.expect("request has id");
        let method = method.expect("checked above");
        let requested_protocol = request
            .pointer("/params/protocolVersion")
            .and_then(Value::as_str);
        let response = match method {
            "initialize"
                if requested_protocol
                    .is_some_and(|version| MCP_PROTOCOL_VERSIONS.contains(&version)) =>
            {
                initialized = true;
                success(
                    id,
                    json!({
                        "protocolVersion": requested_protocol.unwrap(),
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {
                            "name": "dolgorae-live-transport-probe",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }),
                )
            }
            "initialize" => rpc_error(id, -32602, "unsupported MCP protocol version"),
            _ if !initialized => rpc_error(id, -32002, "MCP server is not initialized"),
            "tools/list" => success(id, json!({"tools": [tool_schema()]})),
            "tools/call" => call_tool(id, &request, &binding, &ledger),
            _ => rpc_error(id, -32601, "method not found"),
        };
        write_response(&mut writer, response)?;
    }
}

fn call_tool(
    id: Value,
    request: &Value,
    process: &ProbeProcessBinding,
    ledger: &Mutex<ProbeLedger>,
) -> Value {
    let name = request.pointer("/params/name").and_then(Value::as_str);
    if name != Some(TOOL_NAME) {
        return rpc_error(id, -32601, "tool not found");
    }
    let arguments = request
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Err(error) = reject_model_identity(&arguments) {
        return application_error(id, &error);
    }
    let meta = request.pointer("/params/_meta");
    let context = McpCallContext {
        process_session_id: Some(process.session_id),
        process_run_id: Some(process.run_id),
        process_generation: process.generation,
        lane: process.lane,
        jsonrpc_id: request.get("id").map(ToString::to_string),
        connection_id: None,
        meta_turn_id: meta
            .and_then(|value| value.get(META_TURN))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        meta_tool_call_id: meta
            .and_then(|value| value.get(META_CALL))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        meta_idempotency_key: meta
            .and_then(|value| value.get(META_IDEMPOTENCY))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    };
    let binding = match context.binding() {
        Ok(binding) => binding,
        Err(error) => return application_error(id, &error),
    };
    let mut ledger = ledger.lock().expect("probe ledger mutex");
    match ledger.dispatch(
        &binding,
        &arguments,
        process.generation,
        ProbeInvoke::Complete {
            result: json!({"ok": true}),
        },
    ) {
        Ok(ProbeOutcome::Accepted(receipt) | ProbeOutcome::Replay(receipt)) => success(
            id,
            json!({
                "content": [{"type": "text", "text": receipt.state}],
                "structuredContent": {
                    "state": receipt.state,
                    "payload_sha256": receipt.payload_sha256
                }
            }),
        ),
        Ok(ProbeOutcome::WaitExpired { .. } | ProbeOutcome::InterruptedUnknown) => {
            application_error(
                id,
                &MachineError::new(
                    "ORCHESTRATION_TRANSPORT_UNAVAILABLE",
                    "probe wait or disconnect is not a model-visible MCP result",
                    false,
                    json!({}),
                ),
            )
        }
        Err(error) => application_error(id, &error),
    }
}

fn tool_schema() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": "Isolated TASK-025 transport probe. Identity is bound outside arguments.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["nonce"],
            "properties": {
                "nonce": {"type": "string", "minLength": 1, "maxLength": 64}
            }
        }
    })
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &'static str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn application_error(id: Value, error: &MachineError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32000,
            "message": error.message,
            "data": {"code": error.code, "retryable": error.retryable}
        }
    })
}

fn write_response<W: Write>(writer: &mut W, response: Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, &response)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::new_uuid_v7;
    use std::io::Cursor;

    fn exchange(input: &[u8], dedicated: bool) -> Vec<Value> {
        let mut output = Vec::new();
        let binding = ProbeProcessBinding {
            session_id: new_uuid_v7(),
            run_id: new_uuid_v7(),
            generation: 1,
            lane: if dedicated {
                ExecutionLaneKind::DedicatedLane
            } else {
                ExecutionLaneKind::SharedProfileServer
            },
        };
        serve(
            Cursor::new(input),
            &mut output,
            binding,
            MAX_FRAME_BYTES,
            ProbeLedger::default(),
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("capability"));
        assert!(!text.contains("/private/sockets/"));
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn initialize_and(extra: &str) -> Vec<Value> {
        let mut body = String::from(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2026-07-28\"}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        );
        body.push_str(extra);
        exchange(body.as_bytes(), true)
    }

    #[test]
    fn accepts_the_protocol_version_codex_0_155_sends() {
        let responses = exchange(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\"}}\n",
            true,
        );
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn lists_the_probe_tool_without_identity_fields() {
        let responses = initialize_and(
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
        );
        assert_eq!(responses[0]["result"]["protocolVersion"], "2026-07-28");
        assert_eq!(responses[1]["result"]["tools"][0]["name"], TOOL_NAME);
        let schema = responses[1]["result"]["tools"][0]["inputSchema"].to_string();
        for field in crate::live_transport::TRANSPORT_BOUND_FIELDS {
            assert!(!schema.contains(field), "{field}");
        }
    }

    #[test]
    fn dedicated_lane_without_meta_does_not_invent_turn_identity() {
        let responses = initialize_and(
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"dolgorae_transport_probe\",\"arguments\":{\"nonce\":\"n1\"}}}\n",
        );
        assert_eq!(
            responses[1]["error"]["data"]["code"],
            "ORCHESTRATION_TRANSPORT_UNAVAILABLE"
        );
    }

    #[test]
    fn host_meta_binds_and_replays_without_model_identity() {
        let call = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{{\"name\":\"dolgorae_transport_probe\",\"arguments\":{{\"nonce\":\"n1\"}},\"_meta\":{{\"{META_TURN}\":\"turn-1\",\"{META_CALL}\":\"call-1\",\"{META_IDEMPOTENCY}\":\"idem-1\"}}}}}}\n"
        );
        let mut body = call.clone();
        body.push_str(&call.replace("\"id\":2", "\"id\":3"));
        let responses = initialize_and(&body);
        assert_eq!(
            responses[1]["result"]["structuredContent"]["state"],
            "accepted"
        );
        assert_eq!(
            responses[2]["result"]["structuredContent"]["state"],
            "accepted"
        );
        assert_eq!(
            responses[1]["result"]["structuredContent"]["payload_sha256"],
            responses[2]["result"]["structuredContent"]["payload_sha256"]
        );
    }

    #[test]
    fn model_injected_binding_is_rejected() {
        let responses = initialize_and(
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"dolgorae_transport_probe\",\"arguments\":{\"nonce\":\"n1\",\"source_turn_id\":\"forged\"}}}\n",
        );
        assert_eq!(responses[1]["error"]["data"]["code"], "INVALID_ARGUMENT");
    }
}
